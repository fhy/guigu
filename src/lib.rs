//! guigu：轻量级 Rust 原生 AI Agent 运行时。
//!
//! 顶层 facade：嵌入方 `guigu = { path = ... }` 后可直接用 `guigu::AgentHandle`、
//! `guigu::EchoTool` 等公开项，无需深入 `core` / `tools` 子模块。
//!
//! # Feature
//! - `providers-http`（默认）：启用真实 LLM HTTP 适配器（OpenAI / Anthropic），
//!   依赖 `reqwest`。嵌入方若只需核心运行时，可用
//!   `guigu = { path = ..., default-features = false }` 剥离 HTTP 依赖。

#[cfg(feature = "providers-http")]
pub mod adapters;
pub mod core;
pub mod remote;
pub mod tools;

#[cfg(feature = "providers-http")]
pub use adapters::{AnthropicConfig, AnthropicProvider, OpenAiConfig, OpenAiProvider};
pub use core::{
    agent::*,
    compactor::*,
    context::{CompactionPolicy, ContextBudget, default_convert_to_llm, prepare_context},
    event::*,
    message::*,
    provider::*,
    runtime::*,
    session::*,
    tool::*,
};
pub use remote::{RemoteClient, RemoteError, RemoteRequest, RemoteServer, ServerMessage};
pub use tools::*;
