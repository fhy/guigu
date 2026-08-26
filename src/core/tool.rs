//! Tool trait：工具抽象（参数、执行策略、资源声明）。
//!
//! 一期参数校验宽松：工具内部用 `serde_json::from_value::<T>()` 反序列化，
//! 失败返回 `ToolError`（不 throw）。工具执行失败不中断主循环，错误编码进
//! `ToolResult` / `ToolError` 并最终进入 assistant 上下文（Pi 哲学）。
//!
//! `resource_scope` 决定并发安全性：`ReadOnly` 可与其他 `ReadOnly` 并行；
//! `FileWriter` 之间串行（二期走 file_mutation_queue）；`Exclusive` 独占，
//! 不与任何写工具并行。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::core::message::ToolResultContent;

/// 工具执行结果。
///
/// `is_error` 表达"工具逻辑失败"（区别于 `Result::Err` 的"执行异常"）；
/// 两者最终都会进入 assistant 上下文。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: Vec<ToolResultContent>,
    pub is_error: bool,
    pub details: Option<serde_json::Value>,
}

impl ToolResult {
    /// 构造一个纯文本结果。
    pub fn text(text: impl Into<String>) -> Self {
        ToolResult {
            content: vec![ToolResultContent::Text { text: text.into() }],
            is_error: false,
            details: None,
        }
    }

    /// 构造一个纯文本错误结果。
    pub fn error(text: impl Into<String>) -> Self {
        ToolResult {
            content: vec![ToolResultContent::Text { text: text.into() }],
            is_error: true,
            details: None,
        }
    }
}

/// 工具执行错误（参数非法、执行异常等）。
#[derive(Error, Debug, Clone, Serialize, Deserialize)]
#[error("ToolError: {message}")]
pub struct ToolError {
    pub message: String,
}

impl ToolError {
    /// 参数非法错误。
    pub fn invalid_arguments(message: String) -> Self {
        ToolError { message }
    }

    /// 通用错误。
    pub fn new(message: String) -> Self {
        ToolError { message }
    }
}

/// 资源声明：决定并发安全性。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceScope {
    /// 只读：可与其他 `ReadOnly` 工具并行。
    ReadOnly,
    /// 文件写：与其他 `FileWriter` 串行（二期走 file_mutation_queue）。
    FileWriter,
    /// 独占（如 bash）：不与任何写工具并行。
    Exclusive,
}

/// 工具抽象。
///
/// `execute` 接收已校验的 `args`、run 级 `CancellationToken` 与可选的
/// 增量回调 `on_update`。`on_update` 需 `Send + Sync` 以支持 `ReadOnly`
/// 并行执行时在同一 task 内被多个并发 future 共享。
#[async_trait]
pub trait Tool: Send + Sync {
    /// 工具名（唯一标识，供 LLM toolCall 引用）。
    fn name(&self) -> &str;

    /// 工具描述（供 LLM 理解用途）。
    fn description(&self) -> &str;

    /// 参数 schema（一期宽松，`None` 表示不约束；二期接 schemars）。
    fn parameters(&self) -> Option<serde_json::Value> {
        None
    }

    /// 资源声明：一期用于判定并发安全性。
    fn resource_scope(&self) -> ResourceScope;

    /// 执行工具。失败返回 `Err(ToolError)`；逻辑失败用 `ToolResult::is_error`。
    async fn execute(
        &self,
        tool_call_id: &str,
        args: serde_json::Value,
        signal: CancellationToken,
        on_update: Option<&(dyn Fn(ToolResult) + Send + Sync)>,
    ) -> Result<ToolResult, ToolError>;
}
