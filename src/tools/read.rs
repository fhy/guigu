//! ReadTool：读取文件内容（支持字节 offset/limit 切片）。
//!
//! `ReadOnly` 范围：可与其他 `ReadOnly` 工具并行。
//! 字节切片可能截断多字节字符，一期接受并在 `details` 记录切片参数。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::core::message::ToolResultContent;
use crate::core::tool::{ResourceScope, Tool, ToolError, ToolResult};

/// ReadTool 参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadArgs {
    /// 文件路径。
    pub path: String,
    /// 字节偏移（缺省 0）。
    pub offset: Option<u64>,
    /// 读取字节数（缺省读全文）。
    pub limit: Option<u64>,
}

/// 文件读取工具：读取文件内容，支持字节 offset/limit 切片。
#[derive(Debug, Clone)]
pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read the contents of a file. Supports byte offset and limit slicing."
    }

    fn parameters(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "offset": { "type": "integer", "minimum": 0 },
                "limit": { "type": "integer", "minimum": 1 }
            },
            "required": ["path"]
        }))
    }

    fn resource_scope(&self) -> ResourceScope {
        ResourceScope::ReadOnly
    }

    async fn execute(
        &self,
        _tool_call_id: &str,
        args: serde_json::Value,
        signal: CancellationToken,
        _on_update: Option<&(dyn Fn(ToolResult) + Send + Sync)>,
    ) -> Result<ToolResult, ToolError> {
        if signal.is_cancelled() {
            return Err(ToolError::new(
                "cancelled: read aborted before IO".to_string(),
            ));
        }

        let read_args: ReadArgs = serde_json::from_value(args)
            .map_err(|e| ToolError::invalid_arguments(e.to_string()))?;

        let path = &read_args.path;
        let meta = tokio::fs::metadata(path)
            .await
            .map_err(|e| ToolError::new(format!("read {path}: {e}")))?;
        if !meta.is_file() {
            return Err(ToolError::new(format!("read {path}: not a regular file")));
        }

        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| ToolError::new(format!("read {path}: {e}")))?;
        let content = String::from_utf8(bytes)
            .map_err(|e| ToolError::new(format!("read {path}: invalid UTF-8: {e}")))?;

        let offset = read_args.offset.unwrap_or(0) as usize;
        let start = offset.min(content.len());
        let end = match read_args.limit {
            Some(limit) => offset.saturating_add(limit as usize).min(content.len()),
            None => content.len(),
        };
        let text = content[start..end].to_string();

        let mut details = serde_json::json!({
            "path": path,
            "bytes": text.len(),
        });
        if let Some(offset) = read_args.offset {
            details["offset"] = serde_json::json!(offset);
        }
        if let Some(limit) = read_args.limit {
            details["limit"] = serde_json::json!(limit);
        }

        Ok(ToolResult {
            content: vec![ToolResultContent::Text { text }],
            is_error: false,
            details: Some(details),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ReadTool 名称应为 "read"。
    #[test]
    fn test_read_tool_name() {
        assert_eq!(ReadTool.name(), "read");
    }

    /// ReadTool 应为 ReadOnly 范围。
    #[test]
    fn test_read_tool_resource_scope() {
        assert_eq!(ReadTool.resource_scope(), ResourceScope::ReadOnly);
    }

    /// ReadTool 应声明参数 schema（path 必填）。
    #[test]
    fn test_read_tool_parameters() {
        let params = ReadTool
            .parameters()
            .expect("parameters should be declared");
        assert_eq!(params["type"], "object");
        let required = params["required"]
            .as_array()
            .expect("required should be an array");
        assert!(required.contains(&serde_json::json!("path")));
    }

    /// ReadTool 缺少 path 字段应返回 invalid_arguments。
    #[tokio::test]
    async fn test_read_tool_missing_path() {
        let result = ReadTool
            .execute(
                "call1",
                serde_json::json!({}),
                CancellationToken::new(),
                None,
            )
            .await;
        match result {
            Err(e) => assert!(
                e.message.contains("path"),
                "error should mention missing path, got: {}",
                e.message
            ),
            Ok(_) => panic!("should fail when path is missing"),
        }
    }

    /// ReadTool 在 signal 已取消时应返回取消错误且不执行 IO。
    #[tokio::test]
    async fn test_read_tool_cancelled() {
        let signal = CancellationToken::new();
        signal.cancel();
        // 用不存在的路径：若执行了 IO 会得到 IO 错误而非取消错误。
        let result = ReadTool
            .execute(
                "call1",
                serde_json::json!({ "path": "/nonexistent/guigu-test-never-exists" }),
                signal,
                None,
            )
            .await;
        match result {
            Err(e) => assert!(
                e.message.contains("cancelled"),
                "error should be cancelled, got: {}",
                e.message
            ),
            Ok(_) => panic!("should fail when cancelled"),
        }
    }
}
