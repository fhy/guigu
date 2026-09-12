//! ReadTool：读取文件内容（支持字节 offset/limit 切片）。
//!
//! `ReadOnly` 范围：可与其他 `ReadOnly` 工具并行。
//! 字节切片可能截断多字节字符，一期接受并在 `details` 记录切片参数。
//!
//! 017-b：构造注入 `work_dir`，相对路径 join `work_dir`（`None` 按进程 cwd
//! 解析，保持旧行为）；路径解析在 `execute` 内完成，不隐式依赖进程 cwd。

use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::core::message::ToolResultContent;
use crate::core::tool::{ResourceScope, Tool, ToolError, ToolResult, tool_parameters};
use crate::tools::resolve_tool_path;

/// ReadTool 参数。
///
/// `schema` feature 下 derive `JsonSchema`，`parameters()` 从类型生成 schema
/// （Task 027）；`offset`/`limit` 的数值约束经 `schemars(range)` 对齐 005 手工 JSON。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReadArgs {
    /// 文件路径。
    pub path: String,
    /// 字节偏移（缺省 0）。
    #[cfg_attr(feature = "schema", schemars(range(min = 0)))]
    pub offset: Option<u64>,
    /// 读取字节数（缺省读全文）。
    #[cfg_attr(feature = "schema", schemars(range(min = 1)))]
    pub limit: Option<u64>,
}

/// 文件读取工具：读取文件内容，支持字节 offset/limit 切片。
///
/// 构造注入 `work_dir`（017-b）：相对路径 join `work_dir`（`None` 按进程 cwd
/// 解析，保持旧行为）；绝对路径不变。
#[derive(Debug, Clone)]
pub struct ReadTool {
    work_dir: Option<PathBuf>,
}

impl ReadTool {
    /// 注入工作目录（相对路径锚点；`None` = 按进程 cwd 解析）。
    pub fn new(work_dir: Option<PathBuf>) -> Self {
        ReadTool { work_dir }
    }
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read the contents of a file. Supports byte offset and limit slicing."
    }

    fn parameters(&self) -> Option<serde_json::Value> {
        // Task 031：委托统一入口 `tool_parameters`（`schema` feature 下从 `ReadArgs`
        // derive 生成，剥离后返回 `None`）；`Tool` trait 签名不变。
        tool_parameters::<ReadArgs>()
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

        // 解析一次：归一化绝对路径用于 IO（017-b，不隐式依赖进程 cwd）。
        let path = resolve_tool_path(self.work_dir.as_deref(), &read_args.path);
        let meta = tokio::fs::metadata(&path)
            .await
            .map_err(|e| ToolError::new(format!("read {}: {e}", path.display())))?;
        if !meta.is_file() {
            return Err(ToolError::new(format!(
                "read {}: not a regular file",
                path.display()
            )));
        }

        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| ToolError::new(format!("read {}: {e}", path.display())))?;

        // 严格校验整个文件是有效 UTF-8（拒绝二进制文件，保持旧行为）。
        if std::str::from_utf8(&bytes).is_err() {
            return Err(ToolError::new(format!(
                "read {}: invalid UTF-8",
                path.display()
            )));
        }

        // 在字节层面做 offset/limit 切片（安全，不会 panic），再转文本。
        // from_utf8_lossy 处理多字节边界截断（截断的字符替换为 U+FFFD），
        // 与文件头"字节切片可能截断多字节字符，一期接受"的注释一致。
        let offset = read_args.offset.unwrap_or(0) as usize;
        let start = offset.min(bytes.len());
        let end = match read_args.limit {
            Some(limit) => offset.saturating_add(limit as usize).min(bytes.len()),
            None => bytes.len(),
        };
        let text = String::from_utf8_lossy(&bytes[start..end]).into_owned();

        let mut details = serde_json::json!({
            "path": path.to_string_lossy(),
            "bytes": end - start,
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

    /// 构造无 work_dir 的 ReadTool（测试统一入口，保持旧行为）。
    fn tool() -> ReadTool {
        ReadTool::new(None)
    }

    /// ReadTool 名称应为 "read"。
    #[test]
    fn test_read_tool_name() {
        assert_eq!(tool().name(), "read");
    }

    /// ReadTool 应为 ReadOnly 范围。
    #[test]
    fn test_read_tool_resource_scope() {
        assert_eq!(tool().resource_scope(), ResourceScope::ReadOnly);
    }

    /// ReadTool 应声明参数 schema（path 必填；offset/limit 可选）。
    /// Task 027：`schema` feature 下 schema 从 `ReadArgs` 类型 derive 生成。
    #[cfg(feature = "schema")]
    #[test]
    fn test_read_tool_parameters() {
        let params = tool().parameters().expect("parameters should be declared");
        assert_eq!(params["type"], "object");
        let props = params["properties"]
            .as_object()
            .expect("properties should be an object");
        assert!(props.contains_key("path"));
        assert!(props.contains_key("offset"));
        assert!(props.contains_key("limit"));
        let required = params["required"]
            .as_array()
            .expect("required should be an array");
        assert!(required.contains(&serde_json::json!("path")));
        assert_eq!(required.len(), 1, "only path should be required");
    }

    /// ReadTool 缺少 path 字段应返回 invalid_arguments。
    #[tokio::test]
    async fn test_read_tool_missing_path() {
        let result = tool()
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
        let result = tool()
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

    /// work_dir 生效：相对路径 join work_dir 后读取（017-b）。
    #[tokio::test]
    async fn test_read_tool_work_dir_relative() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), "in work dir").expect("write file");

        let tool = ReadTool::new(Some(dir.path().to_path_buf()));
        let result = tool
            .execute(
                "call1",
                serde_json::json!({ "path": "a.txt" }),
                CancellationToken::new(),
                None,
            )
            .await
            .expect("read should succeed");
        match &result.content[0] {
            ToolResultContent::Text { text } => assert_eq!(
                text, "in work dir",
                "relative path should resolve under work_dir"
            ),
            other => panic!("expected Text content, got {other:?}"),
        }
    }

    /// work_dir 不影响绝对路径（017-b）。
    #[tokio::test]
    async fn test_read_tool_work_dir_absolute_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let abs = dir.path().join("abs.txt");
        std::fs::write(&abs, "abs content").expect("write file");

        let tool = ReadTool::new(Some(dir.path().to_path_buf()));
        let result = tool
            .execute(
                "call1",
                serde_json::json!({ "path": abs.to_string_lossy() }),
                CancellationToken::new(),
                None,
            )
            .await
            .expect("read should succeed");
        match &result.content[0] {
            ToolResultContent::Text { text } => assert_eq!(text, "abs content"),
            other => panic!("expected Text content, got {other:?}"),
        }
    }

    /// 回归（维护巡检问题1）：offset 落在 UTF-8 多字节字符中间时不 panic，
    /// 截断的字符经 from_utf8_lossy 替换为 U+FFFD。
    #[tokio::test]
    async fn test_read_tool_offset_in_multibyte_char() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("chinese.txt");
        // "你好世界" = 4 个中文字符，每个 3 字节，共 12 字节。
        std::fs::write(&path, "你好世界").expect("write file");

        let tool = ReadTool::new(Some(dir.path().to_path_buf()));
        // offset=1 落在第一个中文字符（3 字节）中间。
        let result = tool
            .execute(
                "call1",
                serde_json::json!({ "path": path.to_string_lossy(), "offset": 1 }),
                CancellationToken::new(),
                None,
            )
            .await
            .expect("read should not panic on multibyte boundary");
        match &result.content[0] {
            ToolResultContent::Text { text } => {
                // 首字符被截断（剩 2 个孤立续字节 BD A0）→ 各替换为 U+FFFD，
                // 后 3 个字符完整。
                assert_eq!(text, "\u{FFFD}\u{FFFD}好世界");
            }
            other => panic!("expected Text content, got {other:?}"),
        }
        // details.bytes 应为实际读取的字节数（12 - 1 = 11）。
        assert_eq!(result.details.as_ref().unwrap()["bytes"], 11);
    }

    /// 回归（维护巡检问题1）：limit 落在 UTF-8 多字节字符中间时不 panic，
    /// 截断的字符经 from_utf8_lossy 替换为 U+FFFD。
    #[tokio::test]
    async fn test_read_tool_limit_in_multibyte_char() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("chinese.txt");
        // "你好世界" = 4 个中文字符，每个 3 字节，共 12 字节。
        std::fs::write(&path, "你好世界").expect("write file");

        let tool = ReadTool::new(Some(dir.path().to_path_buf()));
        // limit=4 在第二个中文字符（好，字节 3-5）中间截断（读到字节 0-3）。
        let result = tool
            .execute(
                "call1",
                serde_json::json!({ "path": path.to_string_lossy(), "limit": 4 }),
                CancellationToken::new(),
                None,
            )
            .await
            .expect("read should not panic on multibyte boundary");
        match &result.content[0] {
            ToolResultContent::Text { text } => {
                // 你（3 字节）完整，好的首字节（E5）孤立 → U+FFFD。
                assert_eq!(text, "你\u{FFFD}");
            }
            other => panic!("expected Text content, got {other:?}"),
        }
        // details.bytes 应为实际读取的字节数（4）。
        assert_eq!(result.details.as_ref().unwrap()["bytes"], 4);
    }

    /// 回归（维护巡检问题1）：offset 越界（超过文件长度）时返回空文本，不 panic。
    #[tokio::test]
    async fn test_read_tool_offset_beyond_eof() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("short.txt");
        std::fs::write(&path, "abc").expect("write file");

        let tool = ReadTool::new(Some(dir.path().to_path_buf()));
        // offset=100 远超文件长度（3 字节），应返回空文本。
        let result = tool
            .execute(
                "call1",
                serde_json::json!({ "path": path.to_string_lossy(), "offset": 100 }),
                CancellationToken::new(),
                None,
            )
            .await
            .expect("read should not panic on out-of-bounds offset");
        match &result.content[0] {
            ToolResultContent::Text { text } => assert_eq!(text, ""),
            other => panic!("expected Text content, got {other:?}"),
        }
        assert_eq!(result.details.as_ref().unwrap()["bytes"], 0);
    }
}
