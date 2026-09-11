//! guigu：轻量级 Rust 原生 AI Agent 运行时。
//!
//! 顶层 facade：嵌入方 `guigu = { path = ... }` 后可直接用 `guigu::AgentHandle`、
//! `guigu::EchoTool` 等公开项，无需深入 `core` / `tools` 子模块。
//!
//! # Feature
//! - `providers-http`（默认）：启用真实 LLM HTTP 适配器（OpenAI / Anthropic），
//!   依赖 `reqwest`。嵌入方若只需核心运行时，可用
//!   `guigu = { path = ..., default-features = false }` 剥离 HTTP 依赖。

pub mod acp;
#[cfg(feature = "providers-http")]
pub mod adapters;
pub mod config;
pub mod core;
pub mod plugin;
pub mod remote;
pub mod server;
pub mod tools;

pub use acp::{AcpAgent, AcpClient, AcpError, AcpFsTool, PermissionMode};
#[cfg(feature = "providers-http")]
pub use adapters::{
    AnthropicConfig, AnthropicProvider, OpenAiConfig, OpenAiProvider, build_provider,
};
// Task 022：模型端点配置。数据结构不 gate；`Config`（TOML 解析）gate 在 `config`。
#[cfg(feature = "config")]
pub use config::Config;
pub use config::{GuiguConfig, ModelConfig, Protocol, ProviderConfigError};
pub use core::{
    agent::*,
    compactor::*,
    context::{CompactionPolicy, ContextBudget, default_convert_to_llm, prepare_context},
    event::*,
    // Task 028：跨进程文件锁原语（fs2 flock/LockFileEx，崩溃自释放）。
    file_lock::{FileLock, FileLockError, FileLockGuard},
    message::*,
    provider::*,
    runtime::*,
    session::*,
    tool::*,
};
// Task 027：类型化工具参数 schema helper（feature `schema`，default 开启）。
// 嵌入方 `default-features = false` 剥离后此 re-export 一并移除。
#[cfg(feature = "schema")]
pub use core::schema::{parameters, root_schema, schema_for};
// Task 029：Agent 层插件（生命周期钩子 + 自定义 agent 工厂 + 注册表）。
pub use plugin::{
    AgentFactory, AgentPlugin, AgentPluginError, AgentPluginRegistry, HookContext, HookError,
    LifecycleHooks, MergedHooks, Plugin, PluginError, PluginRegistry, PluginTool,
};
pub use remote::{RemoteClient, RemoteError, RemoteRequest, RemoteServer};
pub use server::{AgentServer, ServerError, ServerMessage, ServerRequest, SessionId};
pub use tools::*;
