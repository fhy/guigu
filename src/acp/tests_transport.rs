//! ACP transport / JSON-RPC 分帧 / request-id 单测（Task 014）。
//!
//! 从 `tests.rs` 拆出（单测试文件 ≤ 30 个 `#[test]` 约束）。含：
//! - `OutboundMessage` / `InboundMessage` 序列化 / 反序列化；
//! - JSON-RPC 分帧 roundtrip；
//! - `RequestId` 解析 / 往返 / 应答路由（string / 负数 id）。
//!
//! pending 清理 / 入站校验 / writer 错误路径测试见 `tests_transport_errors`。

use serde_json::{Value, json};

use crate::acp::jsonrpc::{InboundMessage, OutboundMessage, RequestId};
use crate::acp::stdio_client::StdioClient;
use crate::remote::codec::LineReader;

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
