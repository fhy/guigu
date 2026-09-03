//! `AcpFsTool`：fs 读写经 `AcpClient` 代理（权限隔离），实现 `Tool` trait。
//!
//! 工具不直接访问本地文件系统，而是经 client 代理（`fs/read_text_file` /
//! `fs/write_text_file`），由 client 决定文件访问权限（权限隔离）。执行前按
//! 当前 `PermissionMode` 判定：`default` / `plan` 先向 client 发
//! `session/request_permission`，授权后才执行；`acceptEdits` / `bypassPermissions` /
//! `dontAsk` / `auto` 直接放行。
//!
//! 本任务提供骨架（任务规格边界声明）：工具装配由 015 CLI / 嵌入方注入。

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::acp::types::PermissionOutcome;
use crate::acp::{AcpClient, AcpError, PermissionMode};
use crate::core::message::ToolResultContent;
use crate::core::tool::{ResourceScope, Tool, ToolError, ToolResult};

/// `AcpFsTool` 参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsArgs {
    /// 操作：`read` / `write`。
    pub operation: String,
    /// 文件路径。
    pub path: String,
    /// 写入内容（`write` 时必填）。
    #[serde(default)]
    pub content: Option<String>,
}

/// fs 读写工具：经 `AcpClient` 代理（权限隔离），实现 `Tool` trait。
#[derive(Clone)]
pub struct AcpFsTool {
    /// client 句柄（经其代理 fs 读写 / 请求权限）。
    client: Arc<dyn AcpClient>,
    /// 所属 session id（fs 请求携带）。
    session_id: String,
    /// 当前权限模式（`session/set_mode` 更新）。
    mode: Arc<RwLock<PermissionMode>>,
}

impl AcpFsTool {
    /// 创建 fs 工具（绑定 client / session / 权限模式）。
    pub fn new(
        client: Arc<dyn AcpClient>,
        session_id: String,
        mode: Arc<RwLock<PermissionMode>>,
    ) -> Self {
        Self {
            client,
            session_id,
            mode,
        }
    }
}

#[async_trait]
impl Tool for AcpFsTool {
    fn name(&self) -> &str {
        "fs"
    }

    fn description(&self) -> &str {
        "Read or write a text file via the client (permission-isolated). \
         Use operation \"read\" to read, \"write\" to write."
    }

    fn parameters(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["read", "write"] },
                "path": { "type": "string" },
                "content": { "type": "string" }
            },
            "required": ["operation", "path"]
        }))
    }

    fn resource_scope(&self) -> ResourceScope {
        // 可写 → `FileWriter`（与其他写工具串行，保守安全）。
        ResourceScope::FileWriter
    }

    async fn execute(
        &self,
        tool_call_id: &str,
        args: Value,
        signal: CancellationToken,
        _on_update: Option<&(dyn Fn(ToolResult) + Send + Sync)>,
    ) -> Result<ToolResult, ToolError> {
        if signal.is_cancelled() {
            return Err(ToolError::new(
                "cancelled: fs aborted before IO".to_string(),
            ));
        }

        let fs_args: FsArgs = serde_json::from_value(args)
            .map_err(|e| ToolError::invalid_arguments(e.to_string()))?;

        // 权限判定：`default` / `plan` 先向 client 请求权限。
        let mode = *self.mode.read().await;
        if mode.requires_permission() {
            let outcome = self
                .request_permission(tool_call_id, &fs_args)
                .await
                .map_err(|e| ToolError::new(e.to_string()))?;
            if !outcome.allowed() {
                return Ok(ToolResult::error("permission denied by client"));
            }
        }

        // 经 client 代理执行 fs 操作。
        match fs_args.operation.as_str() {
            "read" => {
                let params = json!({
                    "sessionId": self.session_id,
                    "path": fs_args.path
                });
                let result = self
                    .client
                    .request("fs/read_text_file", params)
                    .await
                    .map_err(|e| ToolError::new(e.to_string()))?;
                let content = result
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                Ok(ToolResult {
                    content: vec![ToolResultContent::Text { text: content }],
                    is_error: false,
                    details: Some(json!({ "path": fs_args.path })),
                })
            }
            "write" => {
                let content = fs_args.content.ok_or_else(|| {
                    ToolError::invalid_arguments("missing content for write".to_string())
                })?;
                let params = json!({
                    "sessionId": self.session_id,
                    "path": fs_args.path,
                    "content": content
                });
                self.client
                    .request("fs/write_text_file", params)
                    .await
                    .map_err(|e| ToolError::new(e.to_string()))?;
                Ok(ToolResult {
                    content: vec![ToolResultContent::Text {
                        text: "ok".to_string(),
                    }],
                    is_error: false,
                    details: Some(json!({ "path": fs_args.path })),
                })
            }
            other => Err(ToolError::invalid_arguments(format!(
                "unknown operation: {other}"
            ))),
        }
    }
}

impl AcpFsTool {
    /// 向 client 请求权限（`session/request_permission`）。
    async fn request_permission(
        &self,
        tool_call_id: &str,
        fs_args: &FsArgs,
    ) -> Result<PermissionOutcome, AcpError> {
        let kind = if fs_args.operation == "read" {
            "read"
        } else {
            "edit"
        };
        let params = json!({
            "sessionId": self.session_id,
            "toolCall": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": tool_call_id,
                "title": format!("{} {}", fs_args.operation, fs_args.path),
                "kind": kind,
                "status": "pending"
            },
            "options": [
                { "optionId": "allow_once", "name": "Allow once", "kind": "allow_once" },
                { "optionId": "reject_once", "name": "Reject", "kind": "reject_once" }
            ]
        });
        let result = self
            .client
            .request("session/request_permission", params)
            .await?;
        Ok(PermissionOutcome::from_value(&result))
    }
}
