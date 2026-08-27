//! 上下文摘要压缩（二期）：`Compactor` trait + `LlmCompactor` 实现。
//!
//! **职责边界**：`Compactor` 只负责「给一批消息 → 产出一条摘要」。哪些消息该
//! 压缩、保留多少、如何替换 transcript，属于 context/runtime 的编排职责
//! （见 `context::prepare_context`），不放入 `Compactor`，保证可独立单测。
//!
//! `LlmCompactor` 持有 `ModelProvider`（003 定稿 trait），用真实 LLM 生成摘要；
//! 代码只依赖 trait，不依赖具体 adapter，测试用 fake provider 驱动、不依赖网络。

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::core::message::{
    AssistantContent, Message, ThinkingLevel, ToolResultContent, UserContent,
};
use crate::core::provider::{
    AssistantEvent, Context, Model, ModelProvider, ProviderError, ProviderRequest,
};

/// 压缩请求：待摘要的旧消息（由调用方选定，非全量 transcript）+ 取消令牌。
#[derive(Debug, Clone)]
pub struct CompactionRequest {
    pub messages: Vec<Arc<Message>>,
    pub signal: CancellationToken,
}

/// 压缩结果：摘要文本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionResult {
    pub summary: String,
}

/// 压缩错误。
#[derive(Debug, Error)]
pub enum CompactionError {
    /// 摘要 LLM 调用失败（透传 003 `ProviderError`）。
    #[error("summary provider error: {0}")]
    Provider(#[from] ProviderError),
    /// signal 取消。
    #[error("compaction cancelled")]
    Cancelled,
    /// 待压缩消息为空。
    #[error("no messages to compact")]
    EmptyInput,
}

/// 摘要压缩器：职责单一——给一批消息，产出一条摘要。
#[async_trait]
pub trait Compactor: Send + Sync {
    /// 对 `req.messages` 生成一条摘要。
    async fn compact(&self, req: CompactionRequest) -> Result<CompactionResult, CompactionError>;
}

/// LLM 摘要压缩器：持有 `ModelProvider`，用 LLM 生成摘要。
pub struct LlmCompactor {
    provider: Arc<dyn ModelProvider>,
    /// 摘要所用模型标识。
    model: Model,
    /// 摘要 system prompt。
    summary_prompt: String,
}

impl LlmCompactor {
    /// 构造：注入 provider + 摘要模型 + 摘要 system prompt。
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        model: Model,
        summary_prompt: impl Into<String>,
    ) -> Self {
        LlmCompactor {
            provider,
            model,
            summary_prompt: summary_prompt.into(),
        }
    }
}

#[async_trait]
impl Compactor for LlmCompactor {
    async fn compact(&self, req: CompactionRequest) -> Result<CompactionResult, CompactionError> {
        if req.messages.is_empty() {
            return Err(CompactionError::EmptyInput);
        }
        if req.signal.is_cancelled() {
            return Err(CompactionError::Cancelled);
        }
        // 构造 provider 请求：system_prompt = 摘要 prompt；messages = 待压缩消息
        // （原样，作为待摘要内容）；tools 为空（摘要无需工具）。
        let request = ProviderRequest {
            model: self.model.clone(),
            context: Context {
                system_prompt: self.summary_prompt.clone(),
                messages: req.messages.iter().map(|m| (**m).clone()).collect(),
                tools: Vec::new(),
            },
            thinking_level: ThinkingLevel::Off,
            session_id: None,
            signal: req.signal.clone(),
        };
        // 外层 Err（建立请求失败）→ CompactionError::Provider（#[from] 透传）。
        let stream = self.provider.stream(request).await?;
        let mut summary = String::new();
        let mut stream = stream;
        loop {
            tokio::select! {
                // 累积期间监听取消 → Cancelled。
                _ = req.signal.cancelled() => {
                    return Err(CompactionError::Cancelled);
                }
                event = stream.next() => {
                    let Some(event) = event else {
                        // 流自然结束（未收到 Done）：返回已累积摘要。
                        break;
                    };
                    match event {
                        AssistantEvent::TextDelta { text } => summary.push_str(&text),
                        AssistantEvent::Done { .. } => break,
                        // 流内失败 → Provider 错误（aborted=true 仍走 Provider 语义）。
                        AssistantEvent::Error { message, aborted } => {
                            let err = if aborted {
                                ProviderError::Aborted
                            } else {
                                ProviderError::Request(message)
                            };
                            return Err(CompactionError::Provider(err));
                        }
                        // 摘要场景不应出现 ThinkingDelta / ToolCall*，忽略。
                        _ => {}
                    }
                }
            }
        }
        Ok(CompactionResult { summary })
    }
}

/// 默认拼接格式：把消息序列化为可读文本（稳定契约，供单测断言）。
///
/// 每条消息一行，消息间用 `\n` 连接：
/// - `User` → `[user] <content>`
/// - `Assistant` → `[assistant] <content>`（`Thinking` 段忽略，`ToolCall` 段省略参数）
/// - `ToolResult` → `[tool_result:<tool_name>] <content>`
///
/// 单条消息内多段 content 用 `\n` 拼接。
pub fn format_messages_for_summary(messages: &[Arc<Message>]) -> String {
    messages
        .iter()
        .map(|m| format_message_line(m))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 单条消息的一行拼接（`format_messages_for_summary` 的内部实现）。
fn format_message_line(msg: &Message) -> String {
    match msg {
        Message::User(u) => {
            let body = u
                .content
                .iter()
                .map(|c| match c {
                    UserContent::Text { text } => text.clone(),
                    UserContent::Image(img) => format!("[image:{}", img.mime_type),
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("[user] {body}")
        }
        Message::Assistant(a) => {
            let body = a
                .content
                .iter()
                .filter_map(|c| match c {
                    AssistantContent::Text { text } => Some(text.clone()),
                    AssistantContent::Thinking { .. } => None,
                    AssistantContent::ToolCall(tc) => Some(format!("tool_call:{}", tc.name)),
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("[assistant] {body}")
        }
        Message::ToolResult(t) => {
            let body = t
                .content
                .iter()
                .map(|c| match c {
                    ToolResultContent::Text { text } => text.clone(),
                    ToolResultContent::Image(img) => format!("[image:{}", img.mime_type),
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("[tool_result:{}] {body}", t.tool_name)
        }
    }
}

#[cfg(test)]
mod tests;
