//! EditTool：把文件中唯一出现的 old_string 替换为 new_string。
//!
//! `FileWriter` 范围：跨 agent 同文件写由注入的 `FileMutationQueue` 串行化
//! （写 IO 在 guard 持有期间执行）。要求 old_string 在文件中唯一；0 处或 >1 处
//! 匹配均为错误。
//!
//! 017-b：构造注入 `work_dir`，相对路径 join `work_dir`（`None` 按进程 cwd
//! 解析，保持旧行为）；路径解析只做一次，解析结果同用于锁 key 与 IO。

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::core::message::ToolResultContent;
use crate::core::tool::{ResourceScope, Tool, ToolError, ToolResult};
use crate::tools::file_mutation_queue::FileMutationQueue;
use crate::tools::resolve_tool_path;

/// EditTool 参数。
///
/// `schema` feature 下 derive `JsonSchema`，`parameters()` 从类型生成 schema
/// （Task 027）；三字段均非 Option → 自动进 required。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EditArgs {
    /// 文件路径。
    pub path: String,
    /// 要查找的字符串（必须在文件中唯一）。
    pub old_string: String,
    /// 替换后的字符串。
    pub new_string: String,
}

/// 文件编辑工具：把文件中唯一出现的 old_string 替换为 new_string。
///
/// 构造时注入 `Arc<FileMutationQueue>`，写 IO 前 acquire 同路径写锁、
/// 写后 RAII 释放，实现跨 agent 同文件写串行化。
///
/// 017-b：构造注入 `work_dir`，相对路径 join `work_dir`（`None` 按进程 cwd
/// 解析，保持旧行为）；绝对路径不变。
#[derive(Debug, Clone)]
pub struct EditTool {
    queue: Arc<FileMutationQueue>,
    work_dir: Option<PathBuf>,
}

impl EditTool {
    /// 注入跨 agent 写锁队列与工作目录（相对路径锚点；`None` = 按进程 cwd 解析）。
    pub fn new(queue: Arc<FileMutationQueue>, work_dir: Option<PathBuf>) -> Self {
        EditTool { queue, work_dir }
    }
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Replace a unique occurrence of old_string with new_string in a file."
    }

    fn parameters(&self) -> Option<serde_json::Value> {
        // Task 027：`schema` feature 下从 `EditArgs` 类型 derive 生成（替代手工 JSON）；
        // 剥离 `schema` 后无类型化 schema，返回 `None`（`Tool` trait 签名不变）。
        #[cfg(feature = "schema")]
        {
            crate::core::schema::parameters::<EditArgs>()
        }
        #[cfg(not(feature = "schema"))]
        {
            None
        }
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

        // 解析一次（017-b）：归一化绝对路径同用于锁 key 与 IO，保证锁 key 与
        // 实际写文件路径一致。
        let path = resolve_tool_path(self.work_dir.as_deref(), &edit_args.path);

        // 可取消 acquire：read-modify-write 全程持锁，避免跨 agent 丢更新。
        let acquired = tokio::select! {
            g = self.queue.acquire(&path) => g,
            _ = signal.cancelled() => {
                return Err(ToolError::new(
                    "cancelled: edit aborted while waiting for file lock".to_string(),
                ));
            }
        };
        // Task 028：跨进程锁获取失败时拒绝进入写临界区（不静默退化为仅进程内锁）。
        let _guard =
            acquired.map_err(|e| ToolError::new(format!("acquire file lock failed: {e}")))?;
        // 拿锁后二次取消检查，消除 acquire 等待期间被取消的竞态。
        if signal.is_cancelled() {
            return Err(ToolError::new(
                "cancelled: edit aborted after acquiring file lock".to_string(),
            ));
        }

        let meta = tokio::fs::metadata(&path)
            .await
            .map_err(|e| ToolError::new(format!("edit {}: {e}", path.display())))?;
        if !meta.is_file() {
            return Err(ToolError::new(format!(
                "edit {}: not a regular file",
                path.display()
            )));
        }

        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| ToolError::new(format!("edit {}: {e}", path.display())))?;
        let content = String::from_utf8(bytes)
            .map_err(|e| ToolError::new(format!("edit {}: invalid UTF-8: {e}", path.display())))?;

        let matches = content.matches(&edit_args.old_string).count();
        if matches == 0 {
            return Err(ToolError::new(format!(
                "edit {}: old_string not found",
                path.display()
            )));
        }
        if matches > 1 {
            return Err(ToolError::new(format!(
                "edit {}: old_string not unique ({matches} matches)",
                path.display()
            )));
        }

        let new_content = content.replacen(&edit_args.old_string, &edit_args.new_string, 1);

        // 写回前再查一次取消，避免取消后仍写。
        if signal.is_cancelled() {
            return Err(ToolError::new(
                "cancelled: edit aborted before write".to_string(),
            ));
        }

        tokio::fs::write(&path, new_content.as_bytes())
            .await
            .map_err(|e| ToolError::new(format!("edit {}: {e}", path.display())))?;

        Ok(ToolResult {
            content: vec![ToolResultContent::Text {
                text: format!("edited {}: replaced 1 occurrence", path.display()),
            }],
            is_error: false,
            details: Some(serde_json::json!({
                "path": path.to_string_lossy(),
                "replaced": 1,
            })),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造带独立写锁队列、无 work_dir 的 EditTool（测试统一入口，保持旧行为）。
    fn tool() -> EditTool {
        EditTool::new(Arc::new(FileMutationQueue::new()), None)
    }

    /// EditTool 名称应为 "edit"。
    #[test]
    fn test_edit_tool_name() {
        assert_eq!(tool().name(), "edit");
    }

    /// EditTool 应为 FileWriter 范围。
    #[test]
    fn test_edit_tool_resource_scope() {
        assert_eq!(tool().resource_scope(), ResourceScope::FileWriter);
    }

    /// EditTool 应声明参数 schema（path/old_string/new_string 必填）。
    /// Task 027：`schema` feature 下 schema 从 `EditArgs` 类型 derive 生成。
    #[cfg(feature = "schema")]
    #[test]
    fn test_edit_tool_parameters() {
        let params = tool().parameters().expect("parameters should be declared");
        assert_eq!(params["type"], "object");
        let props = params["properties"]
            .as_object()
            .expect("properties should be an object");
        assert!(props.contains_key("path"));
        assert!(props.contains_key("old_string"));
        assert!(props.contains_key("new_string"));
        let required = params["required"]
            .as_array()
            .expect("required should be an array");
        assert!(required.contains(&serde_json::json!("path")));
        assert!(required.contains(&serde_json::json!("old_string")));
        assert!(required.contains(&serde_json::json!("new_string")));
        assert_eq!(required.len(), 3, "all three fields should be required");
    }

    /// EditTool 缺少 old_string 字段应返回 invalid_arguments。
    #[tokio::test]
    async fn test_edit_tool_missing_old_string() {
        let result = tool()
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
        let result = tool()
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

    /// work_dir 生效：相对路径 join work_dir 后编辑（017-b）。
    #[tokio::test]
    async fn test_edit_tool_work_dir_relative() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("e.txt"), "foo bar").expect("write file");
        let tool = EditTool::new(
            Arc::new(FileMutationQueue::new()),
            Some(dir.path().to_path_buf()),
        );
        let result = tool
            .execute(
                "call1",
                serde_json::json!({ "path": "e.txt", "old_string": "bar", "new_string": "BAZ" }),
                CancellationToken::new(),
                None,
            )
            .await
            .expect("edit should succeed");
        assert!(!result.is_error);
        let on_disk = std::fs::read_to_string(dir.path().join("e.txt"))
            .expect("file should exist under work_dir");
        assert_eq!(on_disk, "foo BAZ");
    }
}
