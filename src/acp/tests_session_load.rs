//! ACP `session/load` 单测（Task 014 / 017-b）。
//!
//! 从 `tests.rs` 拆出（单文件 ≤ 400 行约束）：`session/load` 基础返回、显式
//! `head` 透传、非法 head 失败后重试（017-b 事务式 load 回归）、`head` 类型
//! 校验。共享工具见 `testutil`。

use std::sync::Arc;

use serde_json::json;

use crate::acp::{AcpAgent, AcpError};
use crate::core::agent::AgentConfig;
use crate::core::message::{
    AssistantContent, AssistantMessage, Message, StopReason, ThinkingLevel, UserContent,
    UserMessage,
};
use crate::core::provider::Model;
use crate::core::runtime::{AgentRuntime, LoopConfig};
use crate::core::session::SessionStorage;
use crate::server::AgentServer;

use super::testutil::{FakeClient, InMemoryStorage, NoopProvider, make_agent};

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
    // "s1" 返回预置存储（有叶 2）；其余返回空存储。017-a 兼容工厂：返回
    // `Arc<dyn SessionStorage>`（无 head 持久化，行为等价 012）。
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
    // "s1" 返回预置存储（有叶 2）；其余返回空存储。017-a 兼容工厂：返回
    // `Arc<dyn SessionStorage>`（无 head 持久化，行为等价 012）。
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
