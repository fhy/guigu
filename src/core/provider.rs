//! ModelProvider 与 AssistantStream（错误两段式）。
//!
//! **错误两段式**（比"所有错误都不返回 Result"更可判定）：
//! - **建立请求失败**（网络不通、认证失败、参数非法）→ 外层 `Result::Err(ProviderError)`
//! - **流内失败**（流建立后的网络断、协议错、模型错）→ 流内 `AssistantEvent::Error`，
//!   主循环据此产出终态 `AssistantMessage { stop_reason: Error }`
//!
//! `AssistantStream` 用 `futures::Stream`；本任务不引入 pin-project-lite。

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::core::message::{AssistantMessage, Message, ThinkingLevel};

/// 模型描述：id + 上下文窗口（token 预算依据）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    /// 上下文窗口（token 数），用于每轮请求前的预算计算。
    pub context_window: u32,
}

/// 工具规格：传给 LLM 的工具描述（name/description/parameters）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Option<serde_json::Value>,
}

/// 传给 provider 的上下文：system_prompt + 消息 + 工具。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Context {
    pub system_prompt: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
}

/// provider 请求。
#[derive(Debug, Clone)]
pub struct ProviderRequest {
    pub model: Model,
    pub context: Context,
    pub thinking_level: ThinkingLevel,
    pub session_id: Option<String>,
    /// run 级取消令牌：贯穿 provider 流、退避等待。
    pub signal: CancellationToken,
}

/// provider 错误（仅"建立请求失败"这一外层阶段）。
///
/// 四类语义（007 补齐，保留既有 `Request`/`Aborted`）：
/// - `Network`：连接失败 / 超时
/// - `HttpStatus`：HTTP 非 2xx（401 认证 / 400 参数 / 429 限流 / 5xx）
/// - `Parse`：SSE / JSON 结构非法
/// - `Build`：请求体构造失败（防御性，正常不应发生）
#[derive(Error, Debug)]
pub enum ProviderError {
    #[error("Provider request failed: {0}")]
    Request(String),
    #[error("Provider request aborted")]
    Aborted,
    #[error("Network error: {0}")]
    Network(String),
    #[error("HTTP status {status}: {body}")]
    HttpStatus { status: u16, body: String },
    #[error("Parse error: {0}")]
    Parse(String),
    #[error("Build error: {0}")]
    Build(String),
}

/// assistant 流：provider 返回的增量事件流。
pub type AssistantStream = Pin<Box<dyn Stream<Item = AssistantEvent> + Send + 'static>>;

/// assistant 流事件（增量）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AssistantEvent {
    /// 文本增量。
    TextDelta { text: String },
    /// 思考增量。
    ThinkingDelta { thinking: String },
    /// 工具调用开始（`arguments` 为初始 JSON 字符串，可为空）。
    ToolCallStart {
        id: String,
        name: String,
        arguments: String,
    },
    /// 工具调用参数增量（流式累积）。
    ToolCallDelta { id: String, arguments_delta: String },
    /// 工具调用结束。
    ToolCallEnd { id: String },
    /// 流正常结束，携带完整 assistant 消息。
    Done { message: AssistantMessage },
    /// 流内失败（两段式的内层）。`aborted` 为 true 表示由取消触发。
    Error { message: String, aborted: bool },
}

/// 模型 provider 抽象。
///
/// `stream` 建立请求失败返回 `Err`；流建立后的一切失败以流内
/// `AssistantEvent::Error` 表达。
#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// 建立一次 assistant 流。
    async fn stream(&self, request: ProviderRequest) -> Result<AssistantStream, ProviderError>;
}
