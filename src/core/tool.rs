use serde::{Deserialize, Serialize};

use crate::core::message::ToolResultContent;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: Vec<ToolResultContent>,
    pub is_error: bool,
    pub details: Option<serde_json::Value>,
}
