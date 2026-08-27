//! EditTool：把文件中唯一出现的 old_string 替换为 new_string。
//!
//! `FileWriter` 范围：与其他 `FileWriter` 工具串行（二期走 file_mutation_queue）。
//! 要求 old_string 在文件中唯一；0 处或 >1 处匹配均为错误。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::core::message::ToolResultContent;
use crate::core::tool::{ResourceScope, Tool, ToolError, ToolResult};

/// EditTool 参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditArgs {
    /// 文件路径。
    pub path: String,
    /// 要查找的字符串（必须在文件中唯一）。
    pub old_string: String,
    /// 替换后的字符串。
    pub new_string: String,
}

/// 文件编辑工具：把文件中唯一出现的 old_string 替换为 new_string。
#[derive(Debug, Clone)]
pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Replace a unique occurrence of old_string with new_string in a file."
    }

    fn parameters(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "old_string": { "type": "string" },
                "new_string": { "type": "string" }
            },
            "required": ["path", "old_string", "new_string"]
        }))
    }

    fn resource_scope(&self) -> ResourceScope {
        ResourceScope::FileWriter
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
                "cancelled: edit aborted before IO".to_string(),
            ));
        }

        let edit_args: EditArgs = serde_json::from_value(args)
            .map_err(|e| ToolError::invalid_arguments(e.to_string()))?;

        let path = &edit_args.path;
        let meta = tokio::fs::metadata(path)
            .await
            .map_err(|e| ToolError::new(format!("edit {path}: {e}")))?;
        if !meta.is_file() {
            return Err(ToolError::new(format!("edit {path}: not a regular file")));
        }

        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| ToolError::new(format!("edit {path}: {e}")))?;
        let content = String::from_utf8(bytes)
            .map_err(|e| ToolError::new(format!("edit {path}: invalid UTF-8: {e}")))?;

        let matches = content.matches(&edit_args.old_string).count();
        if matches == 0 {
            return Err(ToolError::new(format!("edit {path}: old_string not found")));
        }
        if matches > 1 {
            return Err(ToolError::new(format!(
                "edit {path}: old_string not unique ({matches} matches)"
            )));
        }

        let new_content = content.replacen(&edit_args.old_string, &edit_args.new_string, 1);

        // 写回前再查一次取消，避免取消后仍写。
        if signal.is_cancelled() {
            return Err(ToolError::new(
                "cancelled: edit aborted before write".to_string(),
            ));
        }

        tokio::fs::write(path, new_content.as_bytes())
            .await
            .map_err(|e| ToolError::new(format!("edit {path}: {e}")))?;

        Ok(ToolResult {
            content: vec![ToolResultContent::Text {
                text: format!("edited {path}: replaced 1 occurrence"),
            }],
            is_error: false,
            details: Some(serde_json::json!({
                "path": path,
                "replaced": 1,
            })),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// EditTool 名称应为 "edit"。
    #[test]
    fn test_edit_tool_name() {
        assert_eq!(EditTool.name(), "edit");
    }

    /// EditTool 应为 FileWriter 范围。
    #[test]
    fn test_edit_tool_resource_scope() {
        assert_eq!(EditTool.resource_scope(), ResourceScope::FileWriter);
    }

    /// EditTool 应声明参数 schema（path/old_string/new_string 必填）。
    #[test]
    fn test_edit_tool_parameters() {
        let params = EditTool
            .parameters()
            .expect("parameters should be declared");
        assert_eq!(params["type"], "object");
        let required = params["required"]
            .as_array()
            .expect("required should be an array");
        assert!(required.contains(&serde_json::json!("path")));
        assert!(required.contains(&serde_json::json!("old_string")));
        assert!(required.contains(&serde_json::json!("new_string")));
    }

    /// EditTool 缺少 old_string 字段应返回 invalid_arguments。
    #[tokio::test]
    async fn test_edit_tool_missing_old_string() {
        let result = EditTool
            .execute(
                "call1",
                serde_json::json!({ "path": "/nonexistent/guigu-test-x", "new_string": "y" }),
                CancellationToken::new(),
                None,
            )
            .await;
        match result {
            Err(e) => assert!(
                e.message.contains("old_string"),
                "error should mention missing old_string, got: {}",
                e.message
            ),
            Ok(_) => panic!("should fail when old_string is missing"),
        }
    }

    /// EditTool 在 signal 已取消时应返回取消错误且不执行 IO。
    #[tokio::test]
    async fn test_edit_tool_cancelled() {
        let signal = CancellationToken::new();
        signal.cancel();
        let result = EditTool
            .execute(
                "call1",
                serde_json::json!({
                    "path": "/nonexistent/guigu-test-never",
                    "old_string": "a",
                    "new_string": "b"
                }),
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
