use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::core::message::{AssistantMessage, Message, ToolResultMessage};
use crate::core::tool::ToolResult;

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

// 占位类型，实际实现由 provider.rs 决定
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantEvent;
