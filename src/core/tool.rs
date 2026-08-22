use serde::{Deserialize, Serialize};

use crate::core::message::ToolResultContent;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: Vec<ToolResultContent>,
    pub is_error: bool,
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolError {
    pub message: String,
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ToolError: {}", self.message)
    }
}

impl std::error::Error for ToolError {}

impl ToolError {
    pub fn invalid_arguments(message: String) -> Self {
        ToolError { message }
    }
}

pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;

    fn resource_scope(&self) -> ResourceScope;

    fn call(
        &self,
        args: serde_json::Value,
        context: &std::collections::HashMap<String, String>,
    ) -> Result<ToolResult, ToolError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ResourceScope {
    ReadOnly,
    ReadWrite,
}
