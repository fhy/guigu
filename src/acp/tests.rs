//! ACP 方法映射 / 事件映射单测（Task 014）。
//!
//! 覆盖：方法映射（initialize / session/new / session/prompt / session/cancel /
//! set_mode / authenticate）、事件映射（AgentEvent → SessionUpdate）、stopReason
//! 映射、ContentBlock 解析。共享工具见 `testutil`；fs 工具测试见 `tests_fs`；
//! transport / framing / request-id 测试见 `tests_transport`。

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use crate::acp::PermissionMode;
use crate::acp::mapping::{acp_stop_reason, content_blocks_to_messages, map_event_to_update};
use crate::acp::types::{AcpStopReason, ContentBlock, PermissionOutcome};
use crate::acp::{AcpAgent, AcpError};
use crate::core::agent::AgentConfig;
use crate::core::event::AgentEvent;
use crate::core::message::{
    AssistantContent, AssistantMessage, Message, StopReason, ThinkingLevel, UserContent,
    UserMessage,
};
use crate::core::provider::{AssistantEvent, Model};
use crate::core::runtime::{AgentRuntime, LoopConfig};
use crate::core::session::SessionStorage;
use crate::core::tool::ToolResult;
use crate::server::AgentServer;

use super::testutil::{FakeClient, InMemoryStorage, NoopProvider, SlowProvider, make_agent};

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

/// `session/load` 从持久化恢复并返回 `{ sessionId }`（对齐任务规格方法映射表）。
#[tokio::test]
async fn test_session_load_returns_session_id() {
    let agent = make_agent(Arc::new(NoopProvider));
    let client = FakeClient::new();
    let result = agent
        .handle(&client, "session/load", json!({ "sessionId": "s1" }))
        .await
        .expect("session/load");
    assert_eq!(result["sessionId"], "s1");
}

/// `session/load` 显式 `head` 透传（017-b）：合法叶 head 成功；非法 head（不在
/// 树中）返回 server 错误——证明 `head` 字段被透传（若未透传会回退 max 叶而成功）。
#[tokio::test]
async fn test_session_load_explicit_head() {
    // 预置存储：1(user hi, 根) → 2(assistant ok, 叶)。
    let pre = Arc::new(InMemoryStorage::new());
    pre.append(
        None,
        Message::User(UserMessage {
            content: vec![UserContent::Text {
                text: "hi".to_string(),
            }],
            timestamp: 0,
        }),
    )
    .await
    .expect("append 1");
    pre.append(
        Some(1),
        Message::Assistant(AssistantMessage {
            content: vec![AssistantContent::Text {
                text: "ok".to_string(),
            }],
            model: None,
            usage: None,
            stop_reason: Some(StopReason::Completed),
            error_message: None,
            timestamp: 0,
        }),
    )
    .await
    .expect("append 2");

    let server = AgentServer::new();
    server.with_runtime_factory(|| {
        (
            AgentConfig {
                system_prompt: "test".to_string(),
                model: Some("test-model".to_string()),
                thinking_level: ThinkingLevel::Off,
            },
            AgentRuntime {
                provider: Arc::new(NoopProvider),
                tools: Vec::new(),
                loop_config: LoopConfig {
                    model: Model {
                        id: "test-model".to_string(),
                        context_window: 8192,
                    },
                    ..LoopConfig::default()
                },
            },
        )
    });
    // "s1" 返回预置存储（有叶 2）；其余返回空存储。
    server.with_storage_factory(move |id| {
        if id == "s1" {
            pre.clone()
        } else {
            Arc::new(InMemoryStorage::new())
        }
    });
    let agent = AcpAgent::new(server);
    let client = FakeClient::new();

    // 合法 head（叶 2）→ 成功。
    let result = agent
        .handle(
            &client,
            "session/load",
            json!({ "sessionId": "s1", "head": 2 }),
        )
        .await
        .expect("session/load with valid head");
    assert_eq!(result["sessionId"], "s1");

    // 非法 head（空树中不存在 999）→ server 错误（证明 head 被透传）。
    let result = agent
        .handle(
            &client,
            "session/load",
            json!({ "sessionId": "s2", "head": 999 }),
        )
        .await;
    assert!(
        matches!(result, Err(AcpError::Server(_))),
        "invalid head should be a server error, got: {result:?}"
    );
}

/// 回归（017-b 修复）：同一 `sessionId` 先以非法 head `session/load`（失败），
/// 再以合法 head 重试 → 成功。
///
/// 修复前 `session/load` 先注册 session 再校验 head：非法 head 使
/// `resume_lane_from_factory` 返回错误，但 session 已残留在注册表，后续同 id
/// 重试得到 `DuplicateSession`（状态污染）。修复后事务式 load 在**注册前**校验
/// head，非法 head 不注册 session，重试可成功。
#[tokio::test]
async fn test_session_load_invalid_head_then_retry_valid() {
    // 预置存储：1(user hi, 根) → 2(assistant ok, 叶)。
    let pre = Arc::new(InMemoryStorage::new());
    pre.append(
        None,
        Message::User(UserMessage {
            content: vec![UserContent::Text {
                text: "hi".to_string(),
            }],
            timestamp: 0,
        }),
    )
    .await
    .expect("append 1");
    pre.append(
        Some(1),
        Message::Assistant(AssistantMessage {
            content: vec![AssistantContent::Text {
                text: "ok".to_string(),
            }],
            model: None,
            usage: None,
            stop_reason: Some(StopReason::Completed),
            error_message: None,
            timestamp: 0,
        }),
    )
    .await
    .expect("append 2");

    let server = AgentServer::new();
    server.with_runtime_factory(|| {
        (
            AgentConfig {
                system_prompt: "test".to_string(),
                model: Some("test-model".to_string()),
                thinking_level: ThinkingLevel::Off,
            },
            AgentRuntime {
                provider: Arc::new(NoopProvider),
                tools: Vec::new(),
                loop_config: LoopConfig {
                    model: Model {
                        id: "test-model".to_string(),
                        context_window: 8192,
                    },
                    ..LoopConfig::default()
                },
            },
        )
    });
    // "s1" 返回预置存储（有叶 2）；其余返回空存储。
    server.with_storage_factory(move |id| {
        if id == "s1" {
            pre.clone()
        } else {
            Arc::new(InMemoryStorage::new())
        }
    });
    let agent = AcpAgent::new(server);
    let client = FakeClient::new();

    // 同一 sessionId "s1"：先以非法 head（999 不在树中）load → server 错误。
    let result = agent
        .handle(
            &client,
            "session/load",
            json!({ "sessionId": "s1", "head": 999 }),
        )
        .await;
    assert!(
        matches!(result, Err(AcpError::Server(_))),
        "invalid head should be a server error, got: {result:?}"
    );

    // 无状态污染：非法 head 失败后 session 不应残留在注册表。
    let sessions = agent.server().list_sessions().await;
    assert!(
        !sessions.contains(&"s1".to_string()),
        "failed load must not leave a registered session, got: {sessions:?}"
    );

    // 关键回归：同一 sessionId 以合法 head（叶 2）重试 → 成功（修复前会
    // DuplicateSession）。
    let result = agent
        .handle(
            &client,
            "session/load",
            json!({ "sessionId": "s1", "head": 2 }),
        )
        .await
        .expect("retry with valid head should succeed");
    assert_eq!(result["sessionId"], "s1");
}

/// `session/load` 的 `head` 字段存在但非 unsigned integer → JsonRpc 错误
/// （017-b 建议 1：不静默当作未指定，避免拼写/类型错误被悄悄解释为「未指定
/// head」）。`null` 视为未指定（走 max NodeId 叶回退），不报错。
#[tokio::test]
async fn test_session_load_head_wrong_type_errors() {
    let agent = make_agent(Arc::new(NoopProvider));
    let client = FakeClient::new();

    // head 为字符串 → JsonRpc 错误（非静默 None）。
    let result = agent
        .handle(
            &client,
            "session/load",
            json!({ "sessionId": "s1", "head": "2" }),
        )
        .await;
    assert!(
        matches!(result, Err(AcpError::JsonRpc(_))),
        "string head should be a JsonRpc error, got: {result:?}"
    );

    // head 为负数 → JsonRpc 错误（非 unsigned integer）。
    let result = agent
        .handle(
            &client,
            "session/load",
            json!({ "sessionId": "s1", "head": -1 }),
        )
        .await;
    assert!(
        matches!(result, Err(AcpError::JsonRpc(_))),
        "negative head should be a JsonRpc error, got: {result:?}"
    );

    // head 为 null → 视为未指定（None），不报错（空树回退 head None）。
    let result = agent
        .handle(
            &client,
            "session/load",
            json!({ "sessionId": "s1", "head": null }),
        )
        .await
        .expect("null head should be treated as unspecified");
    assert_eq!(result["sessionId"], "s1");
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

/// 事件映射：`TextDelta` → `agent_message_chunk`。
#[test]
fn test_event_mapping_text() {
    let event = AgentEvent::MessageUpdate {
        message: Arc::new(Message::Assistant(AssistantMessage {
            content: vec![],
            model: None,
            usage: None,
            stop_reason: None,
            error_message: None,
            timestamp: 0,
        })),
        assistant_event: AssistantEvent::TextDelta {
            text: "hello".to_string(),
        },
    };
    let update = map_event_to_update(&event).expect("should map");
    assert_eq!(update["sessionUpdate"], "agent_message_chunk");
    assert_eq!(update["content"]["type"], "text");
    assert_eq!(update["content"]["text"], "hello");
}

/// 事件映射：`ThinkingDelta` → `agent_thought_chunk`。
#[test]
fn test_event_mapping_thinking() {
    let event = AgentEvent::MessageUpdate {
        message: Arc::new(Message::Assistant(AssistantMessage {
            content: vec![],
            model: None,
            usage: None,
            stop_reason: None,
            error_message: None,
            timestamp: 0,
        })),
        assistant_event: AssistantEvent::ThinkingDelta {
            thinking: "hmm".to_string(),
        },
    };
    let update = map_event_to_update(&event).expect("should map");
    assert_eq!(update["sessionUpdate"], "agent_thought_chunk");
    assert_eq!(update["content"]["text"], "hmm");
}

/// 事件映射：`ToolExecutionStart` → `tool_call`（status pending）。
#[test]
fn test_event_mapping_tool_call() {
    let event = AgentEvent::ToolExecutionStart {
        tool_call_id: "tc1".to_string(),
        tool_name: "read".to_string(),
        args: json!({ "path": "/tmp/x" }),
    };
    let update = map_event_to_update(&event).expect("should map");
    assert_eq!(update["sessionUpdate"], "tool_call");
    assert_eq!(update["toolCallId"], "tc1");
    assert_eq!(update["kind"], "read");
    assert_eq!(update["status"], "pending");
}

/// 事件映射：`ToolExecutionEnd` → `tool_call_update`（status completed/failed）。
#[test]
fn test_event_mapping_tool_result() {
    let result = ToolResult::text("file content");
    let event = AgentEvent::ToolExecutionEnd {
        tool_call_id: "tc1".to_string(),
        tool_name: "read".to_string(),
        result,
        is_error: false,
    };
    let update = map_event_to_update(&event).expect("should map");
    assert_eq!(update["sessionUpdate"], "tool_call_update");
    assert_eq!(update["toolCallId"], "tc1");
    assert_eq!(update["status"], "completed");
    assert_eq!(update["content"][0]["content"]["text"], "file content");

    // is_error → failed。
    let result = ToolResult::error("boom");
    let event = AgentEvent::ToolExecutionEnd {
        tool_call_id: "tc2".to_string(),
        tool_name: "read".to_string(),
        result,
        is_error: true,
    };
    let update = map_event_to_update(&event).expect("should map");
    assert_eq!(update["status"], "failed");
}

/// 非推送事件（`AgentStart` / `TurnStart` 等）→ `None`。
#[test]
fn test_event_mapping_no_push() {
    assert!(map_event_to_update(&AgentEvent::AgentStart).is_none());
    assert!(map_event_to_update(&AgentEvent::TurnStart).is_none());
    assert!(map_event_to_update(&AgentEvent::AgentEnd { messages: vec![] }).is_none());
}

/// stopReason 映射：各 `StopReason` → ACP `stopReason`。
#[test]
fn test_stop_reason_mapping() {
    assert_eq!(
        acp_stop_reason(&StopReason::Completed),
        AcpStopReason::EndTurn
    );
    assert_eq!(
        acp_stop_reason(&StopReason::Length),
        AcpStopReason::MaxTokens
    );
    assert_eq!(acp_stop_reason(&StopReason::Error), AcpStopReason::Refusal);
    assert_eq!(
        acp_stop_reason(&StopReason::Aborted),
        AcpStopReason::Cancelled
    );
    assert_eq!(
        acp_stop_reason(&StopReason::Pending),
        AcpStopReason::EndTurn
    );
    assert_eq!(
        acp_stop_reason(&StopReason::Other("x".into())),
        AcpStopReason::EndTurn
    );
}

/// `ContentBlock[]` → guigu `Vec<Message>`（文本合并为一条 UserMessage）。
#[test]
fn test_content_blocks_to_messages() {
    let blocks = vec![
        ContentBlock::Text {
            text: "hello".to_string(),
        },
        ContentBlock::Text {
            text: "world".to_string(),
        },
    ];
    let messages = content_blocks_to_messages(&blocks);
    assert_eq!(messages.len(), 1);
    match &messages[0] {
        Message::User(u) => assert_eq!(u.content.len(), 2),
        _ => panic!("expected User message"),
    }
    // 空块 → 空 Vec。
    assert!(content_blocks_to_messages(&[]).is_empty());
}

/// `PermissionOutcome` 解析：selected / cancelled。
#[test]
fn test_permission_outcome_parsing() {
    let selected = PermissionOutcome::from_value(&json!({
        "outcome": { "outcome": "selected", "optionId": "allow_once" }
    }));
    assert!(selected.allowed());

    let cancelled = PermissionOutcome::from_value(&json!({
        "outcome": { "outcome": "cancelled" }
    }));
    assert!(!cancelled.allowed());

    // 非法 → Cancelled。
    let invalid = PermissionOutcome::from_value(&json!({}));
    assert!(!invalid.allowed());
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
