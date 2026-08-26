//! guigu：轻量级 Rust 原生 AI Agent 运行时。
//!
//! 顶层 facade：嵌入方 `guigu = { path = ... }` 后可直接用 `guigu::AgentHandle`、
//! `guigu::EchoTool` 等公开项，无需深入 `core` / `tools` 子模块。

pub mod core;
pub mod tools;

pub use core::{
    agent::*,
    context::{ContextBudget, default_convert_to_llm},
    event::*,
    message::*,
    provider::*,
    runtime::*,
    tool::*,
};
pub use tools::*;
