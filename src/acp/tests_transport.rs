//! ACP transport / JSON-RPC 分帧 / request-id / pending 清理单测（Task 014）。
//!
//! 从 `tests.rs` 拆出（单测试文件 ≤ 30 个 `#[test]` 约束）。含：
//! - `OutboundMessage` / `InboundMessage` 序列化 / 反序列化；
//! - JSON-RPC 分帧 roundtrip；
//! - `RequestId` 解析 / 往返 / 应答路由（string / 负数 id）；
//! - pending 清理（writer 关闭 / `cancel_all`）；
//! - `classify_inbound` 入站消息校验（含缺失 `jsonrpc` 字段，Issue 3）；
//! - writer 写失败 + pending request 回归测试（Issue 1）。

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use serde_json::{Value, json};

use crate::acp::AcpClient;
use crate::acp::jsonrpc::{
    InboundKind, InboundMessage, OutboundMessage, RequestId, classify_inbound,
};
use crate::acp::stdio_client::{StdioClient, StdioConnection};
use crate::remote::codec::LineReader;

use super::testutil::{NoopProvider, make_agent};

// ===== JSON-RPC 分帧（transport）=====

/// `OutboundMessage` 应答（成功）序列化：含 `id` + `result`，无 `method` / `error`。
#[test]
fn test_outbound_result_serialization() {
    let msg = OutboundMessage::result(
        Value::from(1),
        serde_json::json!({"stopReason": "end_turn"}),
    );
    let json = serde_json::to_string(&msg).expect("serialize");
    let v: Value = serde_json::from_str(&json).expect("parse");
    assert_eq!(v["id"], 1);
    assert_eq!(v["result"]["stopReason"], "end_turn");
    assert!(v.get("method").is_none(), "result should not have method");
    assert!(v.get("error").is_none(), "result should not have error");
}

/// `OutboundMessage` 应答（错误）序列化：含 `id` + `error`，无 `result`。
#[test]
fn test_outbound_error_serialization() {
    let msg = OutboundMessage::error(Value::from(2), -32603, "boom".into());
    let json = serde_json::to_string(&msg).expect("serialize");
    let v: Value = serde_json::from_str(&json).expect("parse");
    assert_eq!(v["id"], 2);
    assert_eq!(v["error"]["code"], -32603);
    assert_eq!(v["error"]["message"], "boom");
    assert!(v.get("result").is_none(), "error should not have result");
}

/// `OutboundMessage` notification 序列化：无 `id`，有 `method` + `params`。
#[test]
fn test_outbound_notification_serialization() {
    let msg = OutboundMessage::notification(
        "session/update",
        serde_json::json!({ "sessionId": "s1", "update": {} }),
    );
    let json = serde_json::to_string(&msg).expect("serialize");
    let v: Value = serde_json::from_str(&json).expect("parse");
    assert!(v.get("id").is_none(), "notification should not have id");
    assert_eq!(v["method"], "session/update");
    assert_eq!(v["params"]["sessionId"], "s1");
}

/// `InboundMessage` 请求反序列化：有 `method` + `id` + `params`。
#[test]
fn test_inbound_request_deserialization() {
    let json = r#"{"jsonrpc":"2.0","id":1,"method":"session/new","params":{"cwd":"/tmp"}}"#;
    let msg: InboundMessage = serde_json::from_str(json).expect("parse");
    assert_eq!(msg.method.as_deref(), Some("session/new"));
    assert_eq!(msg.id, Some(Value::from(1)));
    assert_eq!(msg.params.as_ref().unwrap()["cwd"], "/tmp");
    assert!(msg.result.is_none());
    assert!(msg.error.is_none());
}

/// `InboundMessage` notification 反序列化：有 `method`，无 `id`。
#[test]
fn test_inbound_notification_deserialization() {
    let json = r#"{"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s1"}}"#;
    let msg: InboundMessage = serde_json::from_str(json).expect("parse");
    assert_eq!(msg.method.as_deref(), Some("session/cancel"));
    assert!(msg.id.is_none(), "notification should not have id");
}

/// `InboundMessage` 应答反序列化：无 `method`，有 `id` + `result`。
#[test]
fn test_inbound_response_deserialization() {
    let json = r#"{"jsonrpc":"2.0","id":7,"result":{"content":"hello"}}"#;
    let msg: InboundMessage = serde_json::from_str(json).expect("parse");
    assert!(msg.method.is_none(), "response should not have method");
    assert_eq!(msg.id, Some(Value::from(7)));
    assert_eq!(msg.result.as_ref().unwrap()["content"], "hello");
}

/// `InboundMessage` 应答（错误）反序列化：有 `id` + `error`。
#[test]
fn test_inbound_error_response_deserialization() {
    let json = r#"{"jsonrpc":"2.0","id":8,"error":{"code":-32601,"message":"not found"}}"#;
    let msg: InboundMessage = serde_json::from_str(json).expect("parse");
    assert!(msg.method.is_none());
    assert_eq!(msg.id, Some(Value::from(8)));
    assert_eq!(msg.error.as_ref().unwrap().code, -32601);
    assert_eq!(msg.error.as_ref().unwrap().message, "not found");
}

/// JSON-RPC 分帧 roundtrip：`OutboundMessage` 编码后经 `LineReader` 解码还原。
#[tokio::test]
async fn test_jsonrpc_framing_roundtrip() {
    use tokio::io::AsyncWriteExt;

    let (mut client, server) = tokio::io::duplex(4096);
    let mut reader = LineReader::new(server);

    let msgs = vec![
        OutboundMessage::result(Value::from(1), serde_json::json!({"sessionId": "s1"})),
        OutboundMessage::notification(
            "session/update",
            serde_json::json!({ "sessionId": "s1", "update": { "sessionUpdate": "agent_message_chunk" } }),
        ),
        OutboundMessage::error(Value::from(2), -32603, "boom".into()),
    ];
    // 合并为一次写入（模拟多帧到达同一 read buffer）。
    let mut buf = Vec::new();
    for m in &msgs {
        let mut bytes = serde_json::to_vec(m).expect("encode");
        bytes.push(b'\n');
        buf.extend(bytes);
    }
    client.write_all(&buf).await.expect("write");
    client.flush().await.expect("flush");

    for _ in &msgs {
        let decoded = reader
            .next::<InboundMessage>()
            .await
            .expect("read")
            .expect("some");
        assert!(decoded.id.is_some() || decoded.method.is_some());
    }
}

// ===== RequestId（string / 负数 id 路由）=====

/// `RequestId::from_value`：string / 正数 / 负数合法；bool / null / 对象 → `None`。
#[test]
fn test_request_id_from_value() {
    assert_eq!(
        RequestId::from_value(&Value::from("abc")),
        Some(RequestId::String("abc".into()))
    );
    assert_eq!(
        RequestId::from_value(&Value::from(42)),
        Some(RequestId::Number(42))
    );
    assert_eq!(
        RequestId::from_value(&Value::from(-7)),
        Some(RequestId::Number(-7))
    );
    // 非法类型 → None。
    assert_eq!(RequestId::from_value(&Value::Bool(true)), None);
    assert_eq!(RequestId::from_value(&Value::Null), None);
    assert_eq!(RequestId::from_value(&json!({})), None);
    assert_eq!(RequestId::from_value(&json!([1])), None);
}

/// `RequestId::to_value` 往返：string / 负数 id 序列化后与 `from_value` 一致。
#[test]
fn test_request_id_roundtrip() {
    for id in [
        RequestId::String("req-1".into()),
        RequestId::Number(0),
        RequestId::Number(-1),
        RequestId::Number(i64::MAX),
    ] {
        let v = id.to_value();
        assert_eq!(
            RequestId::from_value(&v),
            Some(id.clone()),
            "roundtrip {id:?}"
        );
    }
}

/// 应答路由：字符串 id / 负数 id 都能正确唤醒对应 pending。
#[tokio::test]
async fn test_resolve_pending_string_and_negative_id() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OutboundMessage>();
    let client = StdioClient::new(tx);

    // 字符串 id。
    let s_rx = client
        .insert_pending_for_test(RequestId::String("abc".into()))
        .await;
    client
        .resolve_pending(RequestId::String("abc".into()), Ok(json!({ "ok": true })))
        .await;
    let result = s_rx.await.expect("string id should resolve").expect("ok");
    assert_eq!(result["ok"], true);

    // 负数 id。
    let n_rx = client.insert_pending_for_test(RequestId::Number(-5)).await;
    client
        .resolve_pending(RequestId::Number(-5), Ok(json!({ "neg": true })))
        .await;
    let result = n_rx.await.expect("negative id should resolve").expect("ok");
    assert_eq!(result["neg"], true);

    // 不匹配的 id 不应误唤醒（pending 已清空）。
    assert_eq!(client.pending_len().await, 0);
}

// ===== pending 清理 =====

/// writer 已关闭时 `request` 失败，且 pending entry 被清理（无泄漏）。
#[tokio::test]
async fn test_pending_cleanup_on_send_failure() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<OutboundMessage>();
    let client = StdioClient::new(tx);
    drop(rx); // 关闭 writer 接收端，使 send 失败。

    let result = client.request("fs/read_text_file", json!({})).await;
    assert!(result.is_err(), "should error when writer closed");
    assert_eq!(
        client.pending_len().await,
        0,
        "pending should be cleaned up"
    );
}

/// `cancel_all`：连接断开时全部 pending 以明确错误结束并清空。
#[tokio::test]
async fn test_cancel_all_resolves_pending() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OutboundMessage>();
    let client = StdioClient::new(tx);

    let rx1 = client.insert_pending_for_test(RequestId::Number(1)).await;
    let rx2 = client
        .insert_pending_for_test(RequestId::String("in-flight".into()))
        .await;
    assert_eq!(client.pending_len().await, 2);

    client.cancel_all().await;
    assert_eq!(client.pending_len().await, 0, "pending should be cleared");

    // 两个等待中的请求都以「连接关闭」错误结束（而非永久挂起）。
    let e1 = rx1
        .await
        .expect("rx1 resolved")
        .expect_err("should be cancelled");
    assert!(e1.to_string().contains("connection closed"));
    let e2 = rx2
        .await
        .expect("rx2 resolved")
        .expect_err("should be cancelled");
    assert!(e2.to_string().contains("connection closed"));
}

// ===== 入站消息校验（classify_inbound）=====

/// `classify_inbound`：`jsonrpc` 版本非法 → 错误。
#[test]
fn test_classify_inbound_bad_version() {
    let msg: InboundMessage =
        serde_json::from_str(r#"{"jsonrpc":"1.0","id":1,"method":"initialize"}"#).unwrap();
    let err = classify_inbound(&msg).expect_err("bad version should error");
    assert_eq!(err.1, -32600);
    assert!(err.2.contains("invalid jsonrpc version"));
}

/// `classify_inbound`：缺失 `jsonrpc` 字段 → 错误（Issue 3：必须带 `jsonrpc: "2.0"`）。
#[test]
fn test_classify_inbound_missing_version() {
    let msg: InboundMessage = serde_json::from_str(r#"{"id":1,"method":"initialize"}"#).unwrap();
    let err = classify_inbound(&msg).expect_err("missing jsonrpc should error");
    assert_eq!(err.1, -32600);
    assert!(
        err.2.contains("missing jsonrpc version"),
        "should report missing version, got: {}",
        err.2
    );
}

/// `classify_inbound`：同时含 `method` 与 `result` → 错误。
#[test]
fn test_classify_inbound_method_and_result() {
    let msg: InboundMessage =
        serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","result":{}}"#)
            .unwrap();
    let err = classify_inbound(&msg).expect_err("method+result should error");
    assert_eq!(err.1, -32600);
    assert!(err.2.contains("both method and result/error"));
}

/// `classify_inbound`：应答缺 `result` / `error` → 错误。
#[test]
fn test_classify_inbound_response_missing_body() {
    let msg: InboundMessage = serde_json::from_str(r#"{"jsonrpc":"2.0","id":1}"#).unwrap();
    let err = classify_inbound(&msg).expect_err("response without body should error");
    assert_eq!(err.1, -32600);
    assert!(err.2.contains("missing result and error"));
}

/// `classify_inbound`：应答 id 非法（bool）→ 错误。
#[test]
fn test_classify_inbound_response_bad_id() {
    let msg: InboundMessage =
        serde_json::from_str(r#"{"jsonrpc":"2.0","id":true,"result":{}}"#).unwrap();
    let err = classify_inbound(&msg).expect_err("bad id should error");
    assert_eq!(err.1, -32600);
    assert!(err.2.contains("missing or invalid id"));
}

/// `classify_inbound`：合法请求 / notification / 应答（string id）正确分类。
#[test]
fn test_classify_inbound_valid_messages() {
    // 请求。
    let msg: InboundMessage =
        serde_json::from_str(r#"{"jsonrpc":"2.0","id":7,"method":"session/new","params":{}}"#)
            .unwrap();
    match classify_inbound(&msg).expect("valid request") {
        InboundKind::Request { id, method, .. } => {
            assert_eq!(id, Value::from(7));
            assert_eq!(method, "session/new");
        }
        _ => panic!("expected Request"),
    }

    // notification（无 id）。
    let msg: InboundMessage =
        serde_json::from_str(r#"{"jsonrpc":"2.0","method":"session/cancel"}"#).unwrap();
    assert!(matches!(
        classify_inbound(&msg).expect("valid notification"),
        InboundKind::Notification { .. }
    ));

    // 应答（string id）。
    let msg: InboundMessage =
        serde_json::from_str(r#"{"jsonrpc":"2.0","id":"abc","result":{"ok":true}}"#).unwrap();
    match classify_inbound(&msg).expect("valid response") {
        InboundKind::Response { id, result } => {
            assert_eq!(id, RequestId::String("abc".into()));
            assert!(result.is_ok());
        }
        _ => panic!("expected Response"),
    }
}

// ===== writer 错误路径（Issue 1 回归测试）=====

/// 总是写失败的 `AsyncWrite`（模拟 client 断开 / 管道破裂）。
struct FailingWriter;

impl tokio::io::AsyncWrite for FailingWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "write failed",
        )))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// writer 写失败且存在 pending request → `serve_connection_with` 返回错误（不挂起），
/// 且 in-flight 请求以「连接关闭」错误结束（Issue 1 回归测试）。
#[tokio::test]
async fn test_writer_error_cancels_pending() {
    let agent = make_agent(Arc::new(NoopProvider));
    let conn = StdioConnection::new();
    let client = conn.client();

    // 发起一个 agent→client 请求（创建 pending，等待应答）。
    let client_clone = Arc::clone(&client);
    let request_task =
        tokio::spawn(async move { client_clone.request("fs/read_text_file", json!({})).await });

    // 跑连接：reader 阻塞（不发消息），writer 立即失败。
    let (reader, _writer_end) = tokio::io::duplex(64);
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        agent.serve_connection_with(reader, FailingWriter, conn),
    )
    .await;

    // serve_connection_with 应在超时前返回错误（writer 失败），而非挂起。
    let result = result.expect("should not hang on writer error");
    assert!(result.is_err(), "should return writer error");

    // in-flight 请求应以「连接关闭」错误结束（而非永久挂起），且错误内容可诊断。
    let req_result = request_task.await.expect("request task");
    let err = req_result.expect_err("pending request should be cancelled");
    assert!(
        err.to_string().contains("connection closed"),
        "should be a connection-closed error, got: {err}"
    );
}
