//! ACP 事件 / stopReason / ContentBlock 映射单测（Task 014）。
//!
//! 从 `tests.rs` 拆出（单文件 ≤ 400 行约束）：`AgentEvent` → `SessionUpdate`
//! 映射、`StopReason` → ACP `stopReason` 映射、`ContentBlock[]` → `Vec<Message>`
//! 解析、`PermissionOutcome` 解析。

use std::sync::Arc;

use serde_json::json;

use crate::acp::mapping::{acp_stop_reason, content_blocks_to_messages, map_event_to_update};
use crate::acp::types::{AcpStopReason, ContentBlock, PermissionOutcome};
use crate::core::event::AgentEvent;
use crate::core::message::{AssistantMessage, Message, StopReason};
use crate::core::provider::AssistantEvent;
use crate::core::tool::ToolResult;

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
