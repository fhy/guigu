//! WriteTool：写入文件内容（自动创建父目录，覆盖写）。
//!
//! `FileWriter` 范围：跨 agent 同文件写由注入的 `FileMutationQueue` 串行化
//! （写 IO 在 guard 持有期间执行）。覆盖写；原子写属后续任务。

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::core::message::ToolResultContent;
use crate::core::tool::{ResourceScope, Tool, ToolError, ToolResult};
use crate::tools::file_mutation_queue::FileMutationQueue;

/// WriteTool 参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteArgs {
    /// 文件路径。
    pub path: String,
    /// 要写入的内容。
    pub content: String,
}

/// 文件写入工具：把内容写入文件（自动创建父目录，覆盖已有文件）。
///
/// 构造时注入 `Arc<FileMutationQueue>`，写 IO 前 acquire 同路径写锁、
/// 写后 RAII 释放，实现跨 agent 同文件写串行化。
#[derive(Debug, Clone)]
pub struct WriteTool {
    queue: Arc<FileMutationQueue>,
}

impl WriteTool {
    /// 注入跨 agent 写锁队列。
    pub fn new(queue: Arc<FileMutationQueue>) -> Self {
        WriteTool { queue }
    }
}

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Write content to a file, creating parent directories as needed. Overwrites existing files."
    }

    fn parameters(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" }
            },
            "required": ["path", "content"]
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
                "cancelled: write aborted before IO".to_string(),
            ));
        }

        let write_args: WriteArgs = serde_json::from_value(args)
            .map_err(|e| ToolError::invalid_arguments(e.to_string()))?;

        let path = &write_args.path;

        // 可取消 acquire：等待同路径写锁期间可被 signal 打断。
        let _guard = tokio::select! {
            g = self.queue.acquire(std::path::Path::new(path)) => g,
            _ = signal.cancelled() => {
                return Err(ToolError::new(
                    "cancelled: write aborted while waiting for file lock".to_string(),
                ));
            }
        };
        // 拿锁后二次取消检查，消除 acquire 等待期间被取消的竞态。
        if signal.is_cancelled() {
            return Err(ToolError::new(
                "cancelled: write aborted after acquiring file lock".to_string(),
            ));
        }

        if let Some(parent) = std::path::Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| ToolError::new(format!("write {path}: create parent dir: {e}")))?;
        }

        tokio::fs::write(path, write_args.content.as_bytes())
            .await
            .map_err(|e| ToolError::new(format!("write {path}: {e}")))?;

        let bytes = write_args.content.len();
        Ok(ToolResult {
            content: vec![ToolResultContent::Text {
                text: format!("wrote {bytes} bytes to {path}"),
            }],
            is_error: false,
            details: Some(serde_json::json!({
                "path": path,
                "bytes": bytes,
            })),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造带独立写锁队列的 WriteTool（测试统一入口）。
    fn tool() -> WriteTool {
        WriteTool::new(Arc::new(FileMutationQueue::new()))
    }

    /// WriteTool 名称应为 "write"。
    #[test]
    fn test_write_tool_name() {
        assert_eq!(tool().name(), "write");
    }

    /// WriteTool 应为 FileWriter 范围。
    #[test]
    fn test_write_tool_resource_scope() {
        assert_eq!(tool().resource_scope(), ResourceScope::FileWriter);
    }

    /// WriteTool 应声明参数 schema（path/content 必填）。
    #[test]
    fn test_write_tool_parameters() {
        let params = tool().parameters().expect("parameters should be declared");
        assert_eq!(params["type"], "object");
        let required = params["required"]
            .as_array()
            .expect("required should be an array");
        assert!(required.contains(&serde_json::json!("path")));
        assert!(required.contains(&serde_json::json!("content")));
    }

    /// WriteTool 缺少 content 字段应返回 invalid_arguments。
    #[tokio::test]
    async fn test_write_tool_missing_content() {
        let result = tool()
            .execute(
                "call1",
                serde_json::json!({ "path": "/nonexistent/guigu-test-x" }),
                CancellationToken::new(),
                None,
            )
            .await;
        match result {
            Err(e) => assert!(
                e.message.contains("content"),
                "error should mention missing content, got: {}",
                e.message
            ),
            Ok(_) => panic!("should fail when content is missing"),
        }
    }

    /// WriteTool 在 signal 已取消时应返回取消错误且不执行 IO。
    #[tokio::test]
    async fn test_write_tool_cancelled() {
        let signal = CancellationToken::new();
        signal.cancel();
        let result = tool()
            .execute(
                "call1",
                serde_json::json!({ "path": "/nonexistent/guigu-test-never", "content": "x" }),
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
