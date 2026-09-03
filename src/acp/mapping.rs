//! guigu 内部类型 ↔ ACP wire 映射。
//!
//! - `map_event_to_update`：001 `AgentEvent` → ACP `SessionUpdate`（`session/update` 的
//!   `update` 字段）。text→`agent_message_chunk`、thinking→`agent_thought_chunk`、
//!   工具开始→`tool_call`、工具结束→`tool_call_update`；其余事件不推送。
//! - `acp_stop_reason`：001 `StopReason` → ACP `stopReason`。
//! - `content_blocks_to_messages`：ACP `ContentBlock[]`（prompt 输入）→ guigu `Vec<Message>`。
//!
//! stopReason 映射表（无直接对应者取最相近，wire 值以 ACP 官方 spec 为准）：
//!
//! | guigu `StopReason` | ACP `stopReason` |
//! |---|---|
//! | `Completed` | `end_turn` |
//! | `Length` | `max_tokens` |
//! | `Error` | `refusal` |
//! | `Aborted` | `cancelled` |
//! | `Pending` / `Other` | `end_turn`（最相近兜底） |

use serde_json::json;

use crate::acp::types::AcpStopReason;
use crate::acp::types::ContentBlock;
use crate::core::event::AgentEvent;
use crate::core::message::{Message, StopReason, ToolResultContent, UserContent, UserMessage};
use crate::core::provider::AssistantEvent;
use crate::core::tool::ToolResult;

/// 把一条 `AgentEvent` 映射为 ACP `SessionUpdate`（`session/update` 的 `update` 字段）。
///
/// 返回 `None` 表示该事件不推送 `session/update`（如 `AgentStart` / `TurnStart` /
/// `MessageStart` / `MessageEnd` / `TurnEnd` / `AgentEnd` / 工具参数增量）。
pub fn map_event_to_update(event: &AgentEvent) -> Option<serde_json::Value> {
    match event {
        AgentEvent::MessageUpdate {
            assistant_event, ..
        } => match assistant_event {
            AssistantEvent::TextDelta { text } => Some(json!({
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": text }
            })),
            AssistantEvent::ThinkingDelta { thinking } => Some(json!({
                "sessionUpdate": "agent_thought_chunk",
                "content": { "type": "text", "text": thinking }
            })),
            _ => None,
        },
        AgentEvent::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args,
        } => Some(json!({
            "sessionUpdate": "tool_call",
            "toolCallId": tool_call_id,
            "title": tool_name,
            "kind": tool_kind(tool_name),
            "status": "pending",
            "rawInput": args
        })),
        AgentEvent::ToolExecutionEnd {
            tool_call_id,
            result,
            is_error,
            ..
        } => {
            let status = if *is_error { "failed" } else { "completed" };
            Some(json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": tool_call_id,
                "status": status,
                "content": tool_result_to_content(result)
            }))
        }
        _ => None,
    }
}

/// guigu `StopReason` → ACP `stopReason`（映射表见模块 doc）。
pub fn acp_stop_reason(reason: &StopReason) -> AcpStopReason {
    match reason {
        StopReason::Completed => AcpStopReason::EndTurn,
        StopReason::Length => AcpStopReason::MaxTokens,
        StopReason::Error => AcpStopReason::Refusal,
        StopReason::Aborted => AcpStopReason::Cancelled,
        StopReason::Pending | StopReason::Other(_) => AcpStopReason::EndTurn,
    }
}

/// ACP `ContentBlock[]`（prompt 输入）→ guigu `Vec<Message>`。
///
/// 一期仅文本：把所有 `text` 块合并为**一条** `UserMessage`（多个 `UserContent::Text`）；
/// 无文本块时返回空 `Vec`。
pub fn content_blocks_to_messages(blocks: &[ContentBlock]) -> Vec<Message> {
    let content: Vec<UserContent> = blocks
        .iter()
        .filter_map(|b| {
            b.text().map(|t| UserContent::Text {
                text: t.to_string(),
            })
        })
        .collect();
    if content.is_empty() {
        return Vec::new();
    }
    vec![Message::User(UserMessage {
        content,
        timestamp: 0,
    })]
}

/// 工具名 → ACP `ToolKind`（最小映射，未识别 → `other`）。
fn tool_kind(name: &str) -> &'static str {
    match name {
        "read" => "read",
        "write" | "edit" => "edit",
        "bash" => "execute",
        _ => "other",
    }
}

/// `ToolResult` 文本内容 → ACP `ToolCallContent[]`（`{ type: "content", content: { type: "text", text } }`）。
fn tool_result_to_content(result: &ToolResult) -> serde_json::Value {
    let blocks: Vec<serde_json::Value> = result
        .content
        .iter()
        .filter_map(|c| match c {
            ToolResultContent::Text { text } => Some(json!({
                "type": "content",
                "content": { "type": "text", "text": text }
            })),
            _ => None,
        })
        .collect();
    serde_json::Value::Array(blocks)
}
