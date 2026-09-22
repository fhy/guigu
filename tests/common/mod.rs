//! 测试共享 helper。
#![allow(dead_code)]
#![allow(unused_imports)]

mod fixtures;
mod provider;
mod tools;

pub use fixtures::*;
pub use guigu::Agent;
pub use guigu::core::agent::AgentHandle;
pub use guigu::core::message::{AssistantContent, AssistantMessage, Message, StopReason};
pub use guigu::core::provider::{AssistantEvent, ProviderError};
pub use guigu::core::tool::{ResourceScope, Tool};
pub use guigu::core::{AgentRuntime, LoopConfig, Model, ToolExecutionMode};
pub use provider::*;
pub use std::sync::Arc;
pub use std::sync::atomic::{AtomicUsize, Ordering};
pub use std::time::Duration;
pub use tokio::sync::oneshot;
pub use tools::*;
