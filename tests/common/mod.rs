//! 测试共享 helper。
#![allow(dead_code)]
#![allow(unused_imports)]

mod fixtures;
mod provider;
mod tools;

pub use fixtures::*;
pub use guigu::core::message::{AssistantContent, AssistantMessage, Message, StopReason};
pub use guigu::core::provider::AssistantEvent;
pub use guigu::core::tool::ResourceScope;
pub use guigu::core::{AgentRuntime, LoopConfig, Model};
pub use provider::*;
pub use std::sync::atomic::{AtomicUsize, Ordering};
pub use std::time::Duration;
pub use tokio::sync::oneshot;
pub use tools::*;
