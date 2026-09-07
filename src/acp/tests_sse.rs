//! SSE 传输单元测试（Task 025，feature `acp-sse`）。
//!
//! 覆盖：`SseAcpClient` 的 `request`（pending resolve / 超时）与 `notify`、
//! `SseTransport` 注册表（register / get / remove / count）与断连 pending 清理、
//! per-session 权限模式隔离（多 client 串扰修复验证）。
//!
//! 双工 `request` 的完整 HTTP 链路（agent 发 request → client POST 应答 →
//! pending resolve）由集成测试 `tests/acp_sse.rs` 覆盖；此处验证 `SseAcpClient`
//! 的机制（id 分配 / pending 注册 / SSE 推送 / oneshot 关联 / 超时映射）。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::acp::jsonrpc::RequestId;
use crate::acp::sse_client::{PendingMap, SseAcpClient, SseEvent};
use crate::acp::testutil::{FakeClient, NoopProvider, make_agent};
use crate::acp::transport_sse::SseTransport;
use crate::acp::{AcpClient, AcpError, PermissionMode};

/// 建一个共享 pending 表的 `SseAcpClient` + mpsc 接收端（测试用）。
fn make_client(
    client_id: &str,
    timeout: Duration,
) -> (Arc<SseAcpClient>, mpsc::Receiver<SseEvent>) {
    let (tx, rx) = mpsc::channel::<SseEvent>(16);
    let pending: Arc<Mutex<PendingMap>> = Arc::new(Mutex::new(HashMap::new()));
    let client = Arc::new(SseAcpClient::new(
        client_id.to_string(),
        tx,
        pending,
        timeout,
    ));
    (client, rx)
}

/// `request` 双工机制：分配 id → 注册 pending → 推 SSE request event →
/// 应答命中 pending → oneshot resolve 返回结果。
#[tokio::test]
async fn test_sse_client_request_resolution() {
    let (client, mut rx) = make_client("c1", Duration::from_secs(5));
    let client_task = Arc::clone(&client);

    let req_task = tokio::spawn(async move {
        client_task
            .request("fs/read_text_file", json!({ "path": "/tmp/x" }))
            .await
    });

    // 收到 agent→client request event（带 id）。
    let event = rx.recv().await.expect("should receive request event");
    assert_eq!(event.event, "fs/read_text_file");
    let msg: Value = serde_json::from_str(&event.data).expect("valid json");
    assert_eq!(msg["method"], "fs/read_text_file");
    assert_eq!(msg["params"]["path"], "/tmp/x");
    let id = msg["id"].clone();
    assert!(id.is_number(), "request should carry a numeric id");

    // 模拟 client POST 应答：命中 pending → resolve oneshot。
    let key_id = RequestId::from_value(&id).expect("parse id");
    {
        let pending_map = client.pending_for_test();
        let mut pending = pending_map.lock().await;
        let tx = pending
            .remove(&("c1".to_string(), key_id))
            .expect("pending entry should exist");
        let _ = tx.send(Ok(json!({ "content": "hello" })));
    }

    let result = req_task.await.expect("task join").expect("request ok");
    assert_eq!(result, json!({ "content": "hello" }));
}

/// `request` 超时：client 不应答 → `tokio::time::timeout` 兜底 →
/// `AcpError::Io(TimedOut)`（不新增 `AcpError` 变体）。
#[tokio::test]
async fn test_sse_client_request_timeout() {
    let (client, _rx) = make_client("c1", Duration::from_millis(100));

    let result = client
        .request("fs/read_text_file", json!({ "path": "/tmp/x" }))
        .await;
    match result {
        Err(AcpError::Io(io_err)) => {
            assert_eq!(io_err.kind(), std::io::ErrorKind::TimedOut);
        }
        other => panic!("expected AcpError::Io(TimedOut), got {other:?}"),
    }
}

/// `request` 发送失败（client 已断开，mpsc 接收端 drop）→ 明确错误 + 移除 pending
/// （避免 oneshot 泄漏）。
#[tokio::test]
async fn test_sse_client_request_send_failure() {
    let (client, rx) = make_client("c1", Duration::from_secs(5));
    drop(rx); // 模拟 client 断开：接收端 drop → 通道 closed。

    let result = client.request("fs/read_text_file", json!({})).await;
    assert!(result.is_err(), "send to closed channel should fail");
    // pending 不应残留（发送失败时已移除）。
    assert_eq!(client.pending_for_test().lock().await.len(), 0);
}

/// `notify`：推 SSE notification event（无 id），`data` 为 JSON-RPC notification。
#[tokio::test]
async fn test_sse_client_notify() {
    let (client, mut rx) = make_client("c1", Duration::from_secs(5));

    client
        .notify(
            "session/update",
            json!({ "sessionId": "s1", "update": { "sessionUpdate": "agent_message_chunk" } }),
        )
        .await
        .expect("notify ok");

    let event = rx.recv().await.expect("should receive notify event");
    assert_eq!(event.event, "session/update");
    let msg: Value = serde_json::from_str(&event.data).expect("valid json");
    assert_eq!(msg["method"], "session/update");
    assert!(msg.get("id").is_none(), "notification should have no id");
    assert_eq!(msg["params"]["sessionId"], "s1");
}

/// 注册表：register / get / remove / count。
#[tokio::test]
async fn test_registry_register_get_remove_count() {
    let agent = make_agent(Arc::new(NoopProvider));
    let transport = Arc::new(SseTransport::new(Arc::new(agent)));
    assert_eq!(transport.client_count().await, 0);

    let (client, _rx) = make_client("c1", Duration::from_secs(5));
    transport.register_client("c1", Arc::clone(&client)).await;
    assert_eq!(transport.client_count().await, 1);
    assert!(transport.get_client("c1").await.is_some());
    assert!(transport.get_client("c2").await.is_none());

    transport.remove_client("c1").await;
    assert_eq!(transport.client_count().await, 0);
    assert!(transport.get_client("c1").await.is_none());
}

/// 断连清理：`remove_client` 移除注册表项 + 清空该 client 的 pending（等待中的
/// `request` 以明确错误立即返回，而非永久挂起）。
#[tokio::test]
async fn test_remove_client_drains_pending() {
    let agent = make_agent(Arc::new(NoopProvider));
    let transport = Arc::new(SseTransport::new(Arc::new(agent)));
    let client_id = "c1".to_string();
    let (tx, mut rx) = mpsc::channel::<SseEvent>(16);
    let client = Arc::new(SseAcpClient::new(
        client_id.clone(),
        tx,
        transport.pending_for_test(),
        Duration::from_secs(5),
    ));
    transport
        .register_client(&client_id, Arc::clone(&client))
        .await;

    // 发起 request（注册 pending + 推 SSE event）。
    let req_task =
        tokio::spawn(async move { client.request("fs/read_text_file", json!({})).await });
    // 等到 SSE event → pending 已注册（insert 在 send 之前）。
    let _event = rx.recv().await.expect("request event");

    // 模拟断连：remove_client → 清空 pending。
    transport.remove_client(&client_id).await;

    // 等待中的 request 以明确错误返回（非挂起）。
    let result = req_task.await.expect("task join");
    assert!(
        result.is_err(),
        "pending request should error on disconnect"
    );
    assert_eq!(transport.client_count().await, 0);
}

/// `resolve_pending`：client 应答命中 pending → oneshot resolve；未命中 → 无操作
/// （不 panic）。
#[tokio::test]
async fn test_resolve_pending() {
    let agent = make_agent(Arc::new(NoopProvider));
    let transport = Arc::new(SseTransport::new(Arc::new(agent)));
    let client_id = "c1".to_string();
    let (tx, _rx) = mpsc::channel::<SseEvent>(16);
    let client = Arc::new(SseAcpClient::new(
        client_id.clone(),
        tx,
        transport.pending_for_test(),
        Duration::from_secs(5),
    ));
    transport
        .register_client(&client_id, Arc::clone(&client))
        .await;

    // 手动插入一个 pending entry（模拟 in-flight request）。
    let (ptx, prx) = oneshot::channel::<Result<Value, AcpError>>();
    {
        let pending_map = transport.pending_for_test();
        let mut pending = pending_map.lock().await;
        pending.insert((client_id.clone(), RequestId::Number(42)), ptx);
    }

    // 命中 → resolve。
    transport
        .resolve_pending(&client_id, RequestId::Number(42), Ok(json!({ "ok": true })))
        .await;
    let resolved = prx.await.expect("pending should resolve");
    assert_eq!(resolved.expect("ok result"), json!({ "ok": true }));

    // 未命中（id 不存在）→ 无操作，不 panic。
    transport
        .resolve_pending(&client_id, RequestId::Number(999), Ok(Value::Null))
        .await;
}

/// per-session 权限模式隔离（多 client 串扰修复验证）：两个 session 各自
/// `session/set_mode` 不同模式，互不影响。
#[tokio::test]
async fn test_per_session_mode_isolation() {
    let agent = make_agent(Arc::new(NoopProvider));
    let client = FakeClient::new();

    // 建两个 session（sessionId 唯一）。
    let s1 = agent
        .handle(&client, "session/new", json!({}))
        .await
        .expect("session/new 1");
    let s2 = agent
        .handle(&client, "session/new", json!({}))
        .await
        .expect("session/new 2");
    let id1 = s1["sessionId"].as_str().expect("sessionId 1").to_string();
    let id2 = s2["sessionId"].as_str().expect("sessionId 2").to_string();
    assert_ne!(id1, id2, "sessionIds should be unique");

    // 各自 set_mode 不同模式。
    agent
        .handle(
            &client,
            "session/set_mode",
            json!({ "sessionId": id1, "modeId": "acceptEdits" }),
        )
        .await
        .expect("set_mode 1");
    agent
        .handle(
            &client,
            "session/set_mode",
            json!({ "sessionId": id2, "modeId": "default" }),
        )
        .await
        .expect("set_mode 2");

    // 隔离：各自读到自己的模式，互不串扰。
    let m1 = *agent.mode_for(&id1).await.read().await;
    let m2 = *agent.mode_for(&id2).await.read().await;
    assert_eq!(m1, PermissionMode::AcceptEdits);
    assert_eq!(m2, PermissionMode::Default);
}
