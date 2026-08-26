//! EchoTool：最小示例工具（回显输入消息），用于端到端验证。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::core::message::ToolResultContent;
use crate::core::tool::{ResourceScope, Tool, ToolError, ToolResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EchoArgs {
    pub message: String,
}

/// 回显工具：把 `message` 字段原样返回。
#[derive(Debug, Clone)]
pub struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Echo back the provided message."
    }

    fn parameters(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "message": { "type": "string" }
            },
            "required": ["message"]
        }))
    }

    fn resource_scope(&self) -> ResourceScope {
        ResourceScope::ReadOnly
    }

    async fn execute(
        &self,
        _tool_call_id: &str,
        args: serde_json::Value,
        _signal: CancellationToken,
        _on_update: Option<&(dyn Fn(ToolResult) + Send + Sync)>,
    ) -> Result<ToolResult, ToolError> {
        let echo_args: EchoArgs = serde_json::from_value(args)
            .map_err(|e| ToolError::invalid_arguments(e.to_string()))?;

        Ok(ToolResult {
            content: vec![ToolResultContent::Text {
                text: echo_args.message,
            }],
            is_error: false,
            details: None,
        })
    }
}
