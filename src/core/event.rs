use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::core::message::{AssistantMessage, Message, ToolResultMessage};
use crate::core::tool::ToolResult;

/// assistant 流事件（定义在 `provider`，此处 re-export 保持 `event::AssistantEvent` 路径）。
pub use crate::core::provider::AssistantEvent;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    AgentStart,
    AgentEnd {
        messages: Vec<Arc<Message>>,
    },
    TurnStart,
    TurnEnd {
        message: Arc<AssistantMessage>,
        tool_results: Vec<ToolResultMessage>,
    },
    MessageStart {
        message: Arc<Message>,
    },
    MessageUpdate {
        message: Arc<Message>,
        assistant_event: AssistantEvent,
    },
    MessageEnd {
        message: Arc<Message>,
    },
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        args: serde_json::Value,
    },
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        args: serde_json::Value,
        partial: ToolResult,
    },
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        result: ToolResult,
        is_error: bool,
    },
}
