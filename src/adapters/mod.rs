//! 真实 LLM HTTP 适配器（OpenAI / Anthropic）。
//!
//! 整个模块由 `providers-http` feature 门控：嵌入方 `default-features = false`
//! 即可剥离 reqwest，得到纯核心库。
//!
//! 分层（均为纯逻辑，可脱离网络单测）：
//! - [`sse`]：通用 SSE 解析器（字节流 → 事件枚举）
//! - [`acc`]：流累积状态（text/thinking/tool_calls + 终态信息）
//! - [`stream`]：共享的 SSE → `AssistantStream` 流逻辑（含取消）
//! - [`openai`] / [`anthropic`]：两个 `ModelProvider` 实现
#![cfg(feature = "providers-http")]

pub mod acc;
pub mod anthropic;
pub mod openai;
pub mod sse;
pub mod stream;

pub use anthropic::{AnthropicConfig, AnthropicProvider};
pub use openai::{OpenAiConfig, OpenAiProvider};

/// 构造好的 HTTP 请求（URL + headers + JSON body），供 provider 发送。
pub(crate) struct BuiltRequest {
    /// 完整请求 URL。
    pub url: String,
    /// 请求头（key, value）。
    pub headers: Vec<(String, String)>,
    /// JSON 请求体。
    pub body: serde_json::Value,
}
