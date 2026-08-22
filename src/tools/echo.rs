use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::core::{
    message::ToolResultContent,
    tool::{ResourceScope, Tool, ToolError, ToolResult},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EchoArgs {
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct EchoTool;

impl Tool for EchoTool {
    fn name(&self) -> &'static str {
        "echo"
    }

    fn resource_scope(&self) -> ResourceScope {
        ResourceScope::ReadOnly
    }

    fn call(
        &self,
        args: serde_json::Value,
        _context: &HashMap<String, String>,
    ) -> Result<ToolResult, ToolError> {
        let echo_args: EchoArgs = serde_json::from_value(args)
            .map_err(|e| ToolError::invalid_arguments(e.to_string()))?;

        let content = vec![ToolResultContent::Text {
            text: echo_args.message,
        }];
        Ok(ToolResult {
            content,
            is_error: false,
            details: None,
        })
    }
}
