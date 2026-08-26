//! Task 004 端到端集成测试：打通 AgentHandle → Runtime loop → EchoTool 完整链路。
//!
//! 复用 `tests/common/mod.rs` 的 `FakeProvider`（勿复制），配置**两轮回放**：
//! 1. 工具轮：`ToolCallStart(echo) → ToolCallEnd → Done`，触发 EchoTool 执行
//! 2. 文本轮：`TextDelta("echo: hello") → Done`，产出终态 assistant 文本
//!
//! 断言（snapshot + 事件交叉验证）：
//! - snapshot 含 `User`、含 `ToolResult`（`tool_name=="echo"`）、含终态
//!   `Assistant`（`stop_reason == Some(Completed)`）
//! - 事件为**完整有序序列**：`AgentStart` 开头 → 两次 `TurnStart/TurnEnd`
//!   → `ToolExecutionStart/ToolExecutionEnd` 包裹工具执行 → 末尾 `AgentEnd`，
//!   非"收到过某事件"式松散断言。
//!
//! 同步点：仅 `wait_for_idle`，禁止 `sleep`。

mod common;

use std::sync::Arc;

use guigu::core::event::AgentEvent;
use guigu::core::message::{Message, StopReason, ToolResultContent, UserContent, UserMessage};
use guigu::core::tool::{ResourceScope, Tool};
use guigu::core::{Agent, AgentHandle, ToolExecutionMode};
use guigu::tools::EchoTool;
use tokio_util::sync::CancellationToken;

fn user_msg(text: &str) -> Message {
    Message::User(UserMessage {
        content: vec![UserContent::Text {
            text: text.to_string(),
        }],
        timestamp: 0,
    })
}

/// 端到端：prompt("hello") → 两轮回放（工具轮 + 文本轮）→ 终态。
#[tokio::test]
async fn test_echo_agent_end_to_end() {
    // 两轮回放：工具轮（echo "hello"）+ 文本轮（"echo: hello"）。
    let provider = common::FakeProvider::new(vec![
        common::tool_call_turn("call1", "echo", r#"{"message":"hello"}"#),
        common::text_turn("echo: hello"),
    ]);

    let handle = AgentHandle::spawn(
        common::make_config(),
        common::make_runtime(
            provider,
            vec![Arc::new(EchoTool)],
            ToolExecutionMode::Sequential,
            8192,
        ),
    );

    // prompt 前订阅，确保不丢事件。
    let mut events = handle.subscribe();

    handle
        .prompt(vec![user_msg("hello")])
        .await
        .expect("prompt should succeed");
    handle
        .wait_for_idle()
        .await
        .expect("wait_for_idle should succeed");

    // ---- snapshot 断言 ----
    let snap = handle.snapshot();

    // 含 User 消息（"hello"）。
    assert!(
        snap.messages
            .iter()
            .any(|m| matches!(m.as_ref(), Message::User(_))),
        "snapshot should contain a User message"
    );

    // 含 ToolResult（tool_name == "echo"），且回显内容为 "hello"。
    let tool_result = snap
        .messages
        .iter()
        .find_map(|m| match m.as_ref() {
            Message::ToolResult(tr) => Some(tr),
            _ => None,
        })
        .expect("snapshot should contain a ToolResult message");
    assert_eq!(
        tool_result.tool_name, "echo",
        "ToolResult should come from the echo tool"
    );
    let echoed = tool_result
        .content
        .iter()
        .find_map(|c| match c {
            ToolResultContent::Text { text } => Some(text.clone()),
            _ => None,
        })
        .expect("ToolResult should carry text content");
    assert_eq!(echoed, "hello", "echo tool should return the input message");

    // 终态 Assistant（stop_reason == Completed），文本为 "echo: hello"。
    let last = snap.messages.last().expect("snapshot should have messages");
    match last.as_ref() {
        Message::Assistant(a) => {
            assert_eq!(
                a.stop_reason,
                Some(StopReason::Completed),
                "final assistant message should be Completed"
            );
            let text = a
                .content
                .iter()
                .find_map(|c| match c {
                    guigu::core::message::AssistantContent::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .expect("final assistant should carry text content");
            assert_eq!(
                text, "echo: hello",
                "final assistant text should be the echo reply"
            );
        }
        other => panic!("final message should be Assistant, got {other:?}"),
    }

    // ---- 事件序列断言（完整有序，非"收到过"）----
    let mut received = Vec::new();
    while let Ok(ev) = events.try_recv() {
        received.push(ev);
    }
    // 提取结构骨架（过滤 Message* 流式事件），断言完整有序序列。
    let skeleton: Vec<&str> = received
        .iter()
        .filter_map(|ev| match ev {
            AgentEvent::AgentStart => Some("AgentStart"),
            AgentEvent::AgentEnd { .. } => Some("AgentEnd"),
            AgentEvent::TurnStart => Some("TurnStart"),
            AgentEvent::TurnEnd { .. } => Some("TurnEnd"),
            AgentEvent::ToolExecutionStart { .. } => Some("ToolExecutionStart"),
            AgentEvent::ToolExecutionEnd { .. } => Some("ToolExecutionEnd"),
            _ => None,
        })
        .collect();
    let expected = vec![
        "AgentStart",
        "TurnStart",
        "ToolExecutionStart",
        "ToolExecutionEnd",
        "TurnEnd",
        "TurnStart",
        "TurnEnd",
        "AgentEnd",
    ];
    assert_eq!(
        skeleton, expected,
        "event sequence should be the complete ordered structural sequence"
    );

    // 交叉验证：ToolExecutionStart/End 的 tool_name 均为 "echo"。
    let tool_events: Vec<&str> = received
        .iter()
        .filter_map(|ev| match ev {
            AgentEvent::ToolExecutionStart { tool_name, .. } => Some(tool_name.as_str()),
            AgentEvent::ToolExecutionEnd { tool_name, .. } => Some(tool_name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        tool_events,
        vec!["echo", "echo"],
        "tool execution events should both reference the echo tool"
    );
}

// ---------- EchoTool 单元测试 ----------

/// EchoTool 应返回名称 "echo"。
#[test]
fn test_echo_tool_name() {
    let tool = EchoTool;
    assert_eq!(tool.name(), "echo");
}

/// EchoTool 应有非空描述。
#[test]
fn test_echo_tool_description() {
    let tool = EchoTool;
    assert!(
        !tool.description().is_empty(),
        "description should not be empty"
    );
}

/// EchoTool 应声明参数 schema。
#[test]
fn test_echo_tool_parameters() {
    let tool = EchoTool;
    assert!(tool.parameters().is_some(), "parameters should be declared");
}

/// EchoTool 应为只读资源范围。
#[test]
fn test_echo_tool_resource_scope() {
    let tool = EchoTool;
    assert_eq!(tool.resource_scope(), ResourceScope::ReadOnly);
}

/// EchoTool.execute 应回显输入消息。
#[tokio::test]
async fn test_echo_tool_execute_echoes_message() {
    let tool = EchoTool;
    let args = serde_json::json!({ "message": "hello world" });
    let signal = CancellationToken::new();

    let result = tool
        .execute("call1", args, signal, None)
        .await
        .expect("echo execute should succeed");

    assert!(!result.is_error, "echo should not be an error");
    assert_eq!(result.content.len(), 1, "echo should return 1 content item");
    match &result.content[0] {
        ToolResultContent::Text { text } => {
            assert_eq!(text, "hello world", "echo should return the input message");
        }
        other => panic!("expected Text content, got {other:?}"),
    }
}

/// EchoTool.execute 对空消息应回显空字符串。
#[tokio::test]
async fn test_echo_tool_execute_empty_message() {
    let tool = EchoTool;
    let args = serde_json::json!({ "message": "" });
    let signal = CancellationToken::new();

    let result = tool
        .execute("call1", args, signal, None)
        .await
        .expect("echo execute should succeed");

    assert!(!result.is_error);
    match &result.content[0] {
        ToolResultContent::Text { text } => {
            assert_eq!(text, "", "echo should return empty string for empty input");
        }
        other => panic!("expected Text content, got {other:?}"),
    }
}

/// EchoTool.execute 对缺少 message 字段应返回错误。
#[tokio::test]
async fn test_echo_tool_execute_missing_message() {
    let tool = EchoTool;
    let args = serde_json::json!({});
    let signal = CancellationToken::new();

    let result = tool.execute("call1", args, signal, None).await;
    assert!(
        result.is_err(),
        "echo should fail when message field is missing"
    );
}
