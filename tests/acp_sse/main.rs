//! ACP SSE+HTTP 集成测试（Task 025，feature `acp-sse`）。
//!
//! 起真实 axum server（`serve_sse`）+ reqwest SSE 客户端，覆盖：
//! - 单 client 端到端：initialize → session/new → session/prompt → SSE
//!   `session/update` 事件流 → 最终 `stopReason` 正确。
//! - 多 client 并发：两 client 各建 session，事件流互不串扰、sessionId 唯一、
//!   prompt 各自独立完成。
//! - `POST /message` 应答路径：对 pending 的应答回 `204`（双工 `request` 机制由
//!   单测 `tests_sse` 覆盖；此处验证 HTTP 应答路由不 panic）。
//! - 连接断开清理：client 断开 SSE → registry 移除 → 旧 `client_id` 的 POST 回
//!   `404`（不再推送、无泄漏）。
//!
//! per-session 权限模式隔离由单测 `test_per_session_mode_isolation` 覆盖（集成侧
//! 无 fs 工具可观察模式，故不重复）。helper（`SseClient` / server 启动）见
//! `sse_client` 子模块（单文件 ≤ 400 行约束）。

#![cfg(feature = "acp-sse")]

mod sse_client;

#[path = "../common/mod.rs"]
mod common;

use std::time::Duration;

use serde_json::json;
use sse_client::{SseClient, start_server};

/// 单 client SSE 端到端：initialize → session/new → session/prompt →
/// `session/update` 事件流 → 最终 `stopReason` 正确（`end_turn`）。
#[tokio::test]
async fn test_sse_single_client_full_session() {
    let (addr, server) = start_server().await;
    let mut client = SseClient::connect(addr).await;
    let timeout = Duration::from_secs(10);

    // 1. initialize。
    let status = client
        .post(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": 1 } }))
        .await;
    assert_eq!(status, 202);
    let init = client.wait_response(1, timeout).await;
    assert_eq!(init["result"]["protocolVersion"], 1);
    assert_eq!(init["result"]["agentCapabilities"]["loadSession"], true);
    assert_eq!(init["result"]["authMethods"].as_array().unwrap().len(), 0);

    // 2. session/new。
    let status = client
        .post(json!({ "jsonrpc": "2.0", "id": 2, "method": "session/new", "params": { "cwd": "/tmp" } }))
        .await;
    assert_eq!(status, 202);
    let new_resp = client.wait_response(2, timeout).await;
    let session_id = new_resp["result"]["sessionId"]
        .as_str()
        .expect("sessionId")
        .to_string();

    // 3. session/prompt（双工：run 进行中逐条推 session/update）。
    let (updates, prompt_resp) = client.prompt(&session_id, 3, timeout).await;
    assert!(
        !updates.is_empty(),
        "should receive session/update notifications"
    );
    let has_text_chunk = updates.iter().any(|u| {
        u["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
            && u["params"]["update"]["content"]["text"] == "ok"
    });
    assert!(
        has_text_chunk,
        "should have agent_message_chunk with text 'ok'"
    );
    for u in &updates {
        assert_eq!(u["params"]["sessionId"], session_id);
    }
    assert_eq!(prompt_resp["result"]["stopReason"], "end_turn");

    drop(client);
    server.abort();
}

/// 多 client 并发：两 client 各建 session，sessionId 唯一、事件流互不串扰、
/// prompt 各自独立完成。
#[tokio::test]
async fn test_sse_multi_client_isolation() {
    let (addr, server) = start_server().await;
    let mut client_a = SseClient::connect(addr).await;
    let mut client_b = SseClient::connect(addr).await;
    let timeout = Duration::from_secs(10);

    // 各自 session/new（sessionId 唯一）。
    let new_a = client_a
        .wait_response_after(
            101,
            timeout,
            json!({ "jsonrpc": "2.0", "id": 101, "method": "session/new", "params": {} }),
        )
        .await;
    let new_b = client_b
        .wait_response_after(
            201,
            timeout,
            json!({ "jsonrpc": "2.0", "id": 201, "method": "session/new", "params": {} }),
        )
        .await;
    let session_a = new_a["result"]["sessionId"]
        .as_str()
        .expect("sid a")
        .to_string();
    let session_b = new_b["result"]["sessionId"]
        .as_str()
        .expect("sid b")
        .to_string();
    assert_ne!(session_a, session_b, "sessionIds should be unique");

    // 各自 prompt：事件流互不串扰（A 的 update 全带 session_a，B 的全带 session_b）。
    let (updates_a, resp_a) = client_a.prompt(&session_a, 102, timeout).await;
    let (updates_b, resp_b) = client_b.prompt(&session_b, 202, timeout).await;

    assert!(!updates_a.is_empty(), "client A should get updates");
    assert!(!updates_b.is_empty(), "client B should get updates");
    for u in &updates_a {
        assert_eq!(u["params"]["sessionId"], session_a, "A update cross-talk");
    }
    for u in &updates_b {
        assert_eq!(u["params"]["sessionId"], session_b, "B update cross-talk");
    }
    assert_eq!(resp_a["result"]["stopReason"], "end_turn");
    assert_eq!(resp_b["result"]["stopReason"], "end_turn");

    drop(client_a);
    drop(client_b);
    server.abort();
}

/// `POST /message` 应答路径：对（不存在的）pending 的应答回 `204`，不 panic。
///
/// 双工 `request` 的完整机制（agent 发 request → client POST 应答 → pending
/// resolve / 超时）由单测 `tests_sse` 覆盖；此处验证 HTTP 应答路由。
#[tokio::test]
async fn test_sse_post_response_path() {
    let (addr, server) = start_server().await;
    let client = SseClient::connect(addr).await;

    // 对无 pending 的 id 回应答 → 204（resolve_pending 未命中，无操作）。
    let status = client
        .post(json!({ "jsonrpc": "2.0", "id": 999, "result": { "ok": true } }))
        .await;
    assert_eq!(status, 204, "response to pending should be 204");

    // 缺 X-Client-Id → 400。
    let status = client
        .post_no_client_id(
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
        )
        .await;
    assert_eq!(status, 400, "missing X-Client-Id should be 400");

    drop(client);
    server.abort();
}

/// 连接断开清理：client 断开 SSE → registry 移除 → 旧 `client_id` 的 POST 回
/// `404`（不再推送、无泄漏）。
#[tokio::test]
async fn test_sse_disconnect_cleanup() {
    let (addr, server) = start_server().await;
    let client = SseClient::connect(addr).await;
    let old_client_id = client.client_id().to_string();

    // 断开 SSE（drop client → mpsc receiver drop → watchdog 清理）。
    drop(client);

    // 轮询：旧 client_id 的 POST 最终回 404（registry 已移除）。
    let http = reqwest::Client::new();
    let mut removed = false;
    for _ in 0..200 {
        let status = http
            .post(format!("http://{}/message", addr))
            .header("X-Client-Id", &old_client_id)
            .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }))
            .send()
            .await
            .expect("post")
            .status()
            .as_u16();
        if status == 404 {
            removed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        removed,
        "old client_id should be removed from registry (404)"
    );

    server.abort();
}
