//! ACP transport 错误路径单测（Task 014）。
//!
//! 从 `tests_transport.rs` 拆出（单文件 ≤ 400 行约束）。含：
//! - pending 清理（writer 关闭 / `cancel_all`）；
//! - `classify_inbound` 入站消息校验（含缺失 `jsonrpc` 字段，Issue 3）；
//! - writer 写失败 + pending request 回归测试（Issue 1）。
//!
//! framing / request-id 测试见 `tests_transport`。

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

use super::testutil::{NoopProvider, make_agent};

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
