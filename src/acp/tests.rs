//! ACP 方法映射单测（Task 014）。
//!
//! 覆盖：initialize / session/new / session/prompt / session/cancel / set_mode /
//! authenticate / prompt 内容块校验。`session/load` 测试见 `tests_session_load`；
//! 事件 / stopReason / ContentBlock 映射测试见 `tests_mapping`；fs 工具测试见
//! `tests_fs`；transport / framing / request-id 测试见 `tests_transport`；
//! pending / 入站校验 / writer 错误路径测试见 `tests_transport_errors`。
//! 共享工具见 `testutil`。

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use crate::acp::PermissionMode;

use super::testutil::{FakeClient, NoopProvider, SlowProvider, make_agent};

/// `initialize` 返回合法 `AgentCapabilities`（`loadSession: true`、`authMethods: []`）。
#[tokio::test]
async fn test_initialize_returns_capabilities() {
    let agent = make_agent(Arc::new(NoopProvider));
    let client = FakeClient::new();
    let result = agent
        .handle(&client, "initialize", json!({"protocolVersion": 1}))
        .await
        .expect("initialize");
    assert_eq!(result["protocolVersion"], 1);
    assert_eq!(result["agentCapabilities"]["loadSession"], true);
    assert_eq!(
        result["agentCapabilities"]["promptCapabilities"]["image"],
        false
    );
    assert_eq!(result["authMethods"].as_array().unwrap().len(), 0);
    assert_eq!(result["agentInfo"]["name"], "guigu");
}

/// `session/new` 返回分配的 `sessionId`。
#[tokio::test]
async fn test_session_new_returns_session_id() {
    let agent = make_agent(Arc::new(NoopProvider));
    let client = FakeClient::new();
    let result = agent
        .handle(&client, "session/new", json!({"cwd": "/tmp"}))
        .await
        .expect("session/new");
    let session_id = result["sessionId"].as_str().expect("sessionId");
    assert!(!session_id.is_empty(), "sessionId should not be empty");
}

/// `session/prompt` 收到 `session/update` 序列并返回 `PromptResponse.stopReason`。
#[tokio::test]
async fn test_session_prompt_returns_stop_reason() {
    let agent = make_agent(Arc::new(NoopProvider));
    let client = FakeClient::new();

    let new_result = agent
        .handle(&client, "session/new", json!({}))
        .await
        .expect("session/new");
    let session_id = new_result["sessionId"].as_str().unwrap().to_string();

    let result = agent
        .handle(
            &client,
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": "hi" }]
            }),
        )
        .await
        .expect("session/prompt");
    assert_eq!(result["stopReason"], "end_turn");

    // 应收到 agent_message_chunk 推送（NoopProvider 发一个 TextDelta）。
    let updates = client.calls_with("session/update");
    assert!(!updates.is_empty(), "should receive session/update");
    let has_text_chunk = updates.iter().any(|u| {
        u["update"]["sessionUpdate"] == "agent_message_chunk"
            && u["update"]["content"]["text"] == "ok"
    });
    assert!(has_text_chunk, "should have agent_message_chunk with text");
}

/// `session/cancel` 触发 lane abort（prompt 返回 `stopReason: cancelled`）。
#[tokio::test]
async fn test_session_cancel_aborts_lane() {
    let agent = Arc::new(make_agent(Arc::new(SlowProvider)));
    let client = Arc::new(FakeClient::new());

    let new_result = agent
        .handle(&*client, "session/new", json!({}))
        .await
        .expect("session/new");
    let session_id = new_result["sessionId"].as_str().unwrap().to_string();

    // 后台跑 prompt（SlowProvider 持续发事件）。
    let agent_clone = Arc::clone(&agent);
    let client_clone = Arc::clone(&client);
    let session_id_clone = session_id.clone();
    let prompt_task = tokio::spawn(async move {
        agent_clone
            .handle(
                &*client_clone,
                "session/prompt",
                json!({
                    "sessionId": session_id_clone,
                    "prompt": [{ "type": "text", "text": "hi" }]
                }),
            )
            .await
    });

    // 等 run 启动（收到 session/update）。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if client.has_call("session/update") {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("run should have started");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // 发 cancel。
    agent
        .handle(
            &*client,
            "session/cancel",
            json!({ "sessionId": session_id }),
        )
        .await
        .expect("session/cancel");

    let result = prompt_task.await.expect("prompt task");
    let result = result.expect("prompt should succeed");
    assert_eq!(result["stopReason"], "cancelled");
}

/// `session/set_mode` 更新权限模式。
#[tokio::test]
async fn test_set_mode_updates_permission() {
    let agent = make_agent(Arc::new(NoopProvider));
    let client = FakeClient::new();

    let new_result = agent
        .handle(&client, "session/new", json!({}))
        .await
        .expect("session/new");
    let session_id = new_result["sessionId"].as_str().unwrap().to_string();

    agent
        .handle(
            &client,
            "session/set_mode",
            json!({ "sessionId": session_id, "modeId": "plan" }),
        )
        .await
        .expect("set_mode");

    let mode = *agent.mode_for(&session_id).await.read().await;
    assert_eq!(mode, PermissionMode::Plan);
}

/// `authenticate` 一期不支持 → 返回 `authMethods: []`（对齐任务规格，非 JSON-RPC 错误）。
#[tokio::test]
async fn test_authenticate_returns_empty_auth_methods() {
    let agent = make_agent(Arc::new(NoopProvider));
    let client = FakeClient::new();
    let result = agent
        .handle(&client, "authenticate", json!({ "methodId": "token" }))
        .await
        .expect("authenticate should succeed (not error)");
    assert_eq!(result["authMethods"].as_array().unwrap().len(), 0);
}

/// 多 session 权限隔离：session A 设 `plan` 不影响 session B（仍 `default`）。
#[tokio::test]
async fn test_set_mode_session_isolation() {
    let agent = make_agent(Arc::new(NoopProvider));
    let client = FakeClient::new();

    let a = agent
        .handle(&client, "session/new", json!({}))
        .await
        .expect("new A");
    let a_id = a["sessionId"].as_str().unwrap().to_string();
    let b = agent
        .handle(&client, "session/new", json!({}))
        .await
        .expect("new B");
    let b_id = b["sessionId"].as_str().unwrap().to_string();

    // session A 设 plan。
    agent
        .handle(
            &client,
            "session/set_mode",
            json!({ "sessionId": a_id, "modeId": "plan" }),
        )
        .await
        .expect("set A");

    // A 是 plan，B 仍是 default（隔离，未串改）。
    let a_mode = *agent.mode_for(&a_id).await.read().await;
    let b_mode = *agent.mode_for(&b_id).await.read().await;
    assert_eq!(a_mode, PermissionMode::Plan);
    assert_eq!(b_mode, PermissionMode::Default);
}

/// `set_mode` 对不存在的 session 返回错误（不修改任何权限状态）。
#[tokio::test]
async fn test_set_mode_unknown_session_errors() {
    let agent = make_agent(Arc::new(NoopProvider));
    let client = FakeClient::new();
    let result = agent
        .handle(
            &client,
            "session/set_mode",
            json!({ "sessionId": "nonexistent", "modeId": "plan" }),
        )
        .await;
    assert!(result.is_err(), "should error for unknown session");
    // 不存在的 session 不应在 modes 表中留下任何状态。
    let mode = *agent.mode_for("nonexistent").await.read().await;
    assert_eq!(
        mode,
        PermissionMode::Default,
        "unknown session mode untouched"
    );
}

/// `set_mode` 缺 `sessionId` → 错误（session 级操作必须携带 sessionId）。
#[tokio::test]
async fn test_set_mode_missing_session_id_errors() {
    let agent = make_agent(Arc::new(NoopProvider));
    let client = FakeClient::new();
    let result = agent
        .handle(&client, "session/set_mode", json!({ "modeId": "plan" }))
        .await;
    assert!(result.is_err(), "should error when sessionId missing");
}

/// `session/prompt` 含非文本块（image）→ 明确「不支持内容类型」错误（非泛化 serde 错误）。
#[tokio::test]
async fn test_prompt_unsupported_content_type() {
    let agent = make_agent(Arc::new(NoopProvider));
    let client = FakeClient::new();

    let new_result = agent
        .handle(&client, "session/new", json!({}))
        .await
        .expect("session/new");
    let session_id = new_result["sessionId"].as_str().unwrap().to_string();

    let result = agent
        .handle(
            &client,
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{ "type": "image", "data": "xxx", "mimeType": "image/png" }]
            }),
        )
        .await;
    let err = result.expect_err("image block should be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("unsupported content type"),
        "should be a clear unsupported-type error, got: {msg}"
    );
    assert!(msg.contains("image"), "should name the offending type");
}

/// `session/prompt` 块缺 `type` 字段 → 明确「非法内容块」错误。
#[tokio::test]
async fn test_prompt_block_missing_type() {
    let agent = make_agent(Arc::new(NoopProvider));
    let client = FakeClient::new();

    let new_result = agent
        .handle(&client, "session/new", json!({}))
        .await
        .expect("session/new");
    let session_id = new_result["sessionId"].as_str().unwrap().to_string();

    let result = agent
        .handle(
            &client,
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{ "text": "no type field" }]
            }),
        )
        .await;
    let err = result.expect_err("block without type should be rejected");
    assert!(err.to_string().contains("missing or non-string 'type'"));
}
