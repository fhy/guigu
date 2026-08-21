use serde::{Deserialize, Serialize};

use crate::core::message::{AssistantMessage, Message, ToolResultContent, ToolResultMessage};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    AgentStart,
    AgentEnd {
        messages: Vec<Message>,
    },
    TurnStart,
    TurnEnd {
        message: AssistantMessage,
        tool_results: Vec<ToolResultMessage>,
    },
    MessageStart {
        message: Message,
    },
    MessageUpdate {
        message: Message,
        assistant_event: AssistantEvent,
    },
    MessageEnd {
        message: Message,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: Vec<ToolResultContent>,
    pub is_error: bool,
    pub details: Option<serde_json::Value>,
}
