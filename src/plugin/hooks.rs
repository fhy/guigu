//! 生命周期钩子：主循环生命周期钩子的 trait 化抽象（Task 029）。
//!
//! [`LifecycleHooks`] 对齐 003 [`LoopConfig`](crate::core::runtime::LoopConfig) 钩子语义，
//! **能力对等**（含改写 / 注入，不弱化）：
//! - `before_tool_call`：`Err` = 否决该工具（按 `ToolError` 语义，不执行工具体）。
//! - `after_tool_call`：按值收 / 返，可改写 `ToolResult`（默认 `Ok(result)` 透传）。
//! - `should_stop_after_turn`：`Some(true)` = 本轮后停止。
//! - `prepare_next_turn`：返回要注入的 `Vec<Message>`（默认空 = 不注入）。
//!
//! 所有方法默认空实现，插件按需覆盖。[`MergedHooks`] 组合多个插件钩子
//! （按 id 字典序链式调用，任一 `Err` 短路）。
//!
//! 边界：`convert_to_llm` / `transform_context` 不开放为插件钩子（上下文投影 /
//! 裁剪属核心逻辑，开放会破坏上下文一致性）。
//!
//! 模块拆分（单文件 ≤ 400 行约束）：单测在 `hooks/tests.rs`（默认实现 + 单 hook
//! 行为）、`hooks/tests_merged.rs`（before/after 合并语义）、
//! `hooks/tests_merged_stop.rs`（should_stop / prepare_next_turn 合并语义）。

use std::sync::Arc;

use async_trait::async_trait;
use thiserror::Error;

use crate::core::message::{AssistantMessage, Message, ToolCall, ToolResultMessage};
use crate::core::tool::ToolResult;

/// 钩子上下文：承载主循环当前可用状态（只读）。
///
/// 钩子只读上下文，不得直接改 transcript。具体钩子数据（tool_call / args /
/// result / assistant / tool_results）由各方法参数单独传递，此处仅承载跨钩子
/// 共享的 transcript 快照。
#[derive(Debug, Clone)]
pub struct HookContext {
    /// 当前 transcript 快照（只读视图，浅拷贝 `Arc` 指针）。
    pub transcript: Vec<Arc<Message>>,
}

impl HookContext {
    /// 从 transcript 切片构造上下文（浅拷贝 `Arc` 指针，不深拷贝消息）。
    pub fn new(transcript: &[Arc<Message>]) -> Self {
        Self {
            transcript: transcript.to_vec(),
        }
    }
}

/// 钩子错误。
///
/// 传播策略（对齐 003 主循环既有钩子失败处理）：
/// - `before_tool_call` 失败 = 否决该工具调用（不执行工具体）。
/// - `after_tool_call` 失败 = 保留原始 `ToolResult`（不改写）、记录日志、不阻断主循环。
/// - `prepare_next_turn` 失败 = 不注入消息、记录日志、不阻断主循环。
#[derive(Debug, Error, Clone)]
#[error("HookError: {message}")]
pub struct HookError {
    pub message: String,
}

impl HookError {
    /// 构造一个钩子错误。
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Agent 主循环生命周期钩子。所有方法默认空实现，插件按需覆盖。
///
/// 语义对齐 003 [`LoopConfig`](crate::core::runtime::LoopConfig) 钩子，能力对等。
#[async_trait]
pub trait LifecycleHooks: Send + Sync {
    /// 工具执行前（对齐 `LoopConfig::before_tool_call`）。
    ///
    /// `Err` = 否决该工具（按 `ToolError` 语义，不执行工具体）。
    async fn before_tool_call(
        &self,
        _ctx: &HookContext,
        _args: &serde_json::Value,
    ) -> Result<(), HookError> {
        Ok(())
    }

    /// 工具执行后（对齐 `LoopConfig::after_tool_call` 的
    /// `Fn(&ToolCall, ToolResult) -> ToolResult`，可改写）。
    ///
    /// 按值收 / 返：`Ok(r)` = 采用 `r`（默认 `Ok(result)` 原样透传 = 不改写）；
    /// `Err` = 保留原始 `result`、记录日志、不阻断主循环。
    async fn after_tool_call(
        &self,
        _ctx: &HookContext,
        _tool_call: &ToolCall,
        result: ToolResult,
    ) -> Result<ToolResult, HookError> {
        Ok(result)
    }

    /// 本轮是否提前结束（对齐 `LoopConfig::should_stop_after_turn`）。
    ///
    /// `Some(true)` = 本轮后停止；`None` = 不干预。
    fn should_stop_after_turn(&self, _ctx: &HookContext) -> Option<bool> {
        None
    }

    /// 下一轮准备（对齐 `LoopConfig::prepare_next_turn` 的
    /// `Fn(&AssistantMessage, &[ToolResultMessage]) -> Vec<Message>`，可注入）。
    ///
    /// 返回要注入的额外消息：空 = 不注入；`Err` = 不注入、记录日志、不阻断主循环。
    async fn prepare_next_turn(
        &self,
        _ctx: &HookContext,
        _assistant: &AssistantMessage,
        _tool_results: &[ToolResultMessage],
    ) -> Result<Vec<Message>, HookError> {
        Ok(Vec::new())
    }
}

/// 组合多个插件钩子（按 id 字典序链式调用）。
///
/// 合并语义：
/// - `before_tool_call`：任一 `Err` 短路（后续插件不调用）。
/// - `after_tool_call`：前一插件返回值作为后一插件 `result` 入参（按值串接）；
///   任一 `Err` 短路，采用最后一个成功值。
/// - `should_stop_after_turn`：首个 `Some(true)` 即停。
/// - `prepare_next_turn`：各插件返回的 `Vec<Message>` 按 id 字典序拼接；
///   任一 `Err` 短路，保留已成功部分。
pub struct MergedHooks {
    hooks: Vec<(String, Arc<dyn LifecycleHooks>)>,
}

impl MergedHooks {
    /// 按 id 字典序排序后组合。
    pub fn new(hooks: Vec<(String, Arc<dyn LifecycleHooks>)>) -> Self {
        let mut hooks = hooks;
        hooks.sort_by(|a, b| a.0.cmp(&b.0));
        Self { hooks }
    }
}

#[async_trait]
impl LifecycleHooks for MergedHooks {
    async fn before_tool_call(
        &self,
        ctx: &HookContext,
        args: &serde_json::Value,
    ) -> Result<(), HookError> {
        for (_id, hook) in &self.hooks {
            hook.before_tool_call(ctx, args).await?;
        }
        Ok(())
    }

    async fn after_tool_call(
        &self,
        ctx: &HookContext,
        tool_call: &ToolCall,
        result: ToolResult,
    ) -> Result<ToolResult, HookError> {
        let mut last_success = result;
        for (_id, hook) in &self.hooks {
            match hook
                .after_tool_call(ctx, tool_call, last_success.clone())
                .await
            {
                Ok(r) => last_success = r,
                Err(_) => return Ok(last_success),
            }
        }
        Ok(last_success)
    }

    fn should_stop_after_turn(&self, ctx: &HookContext) -> Option<bool> {
        for (_id, hook) in &self.hooks {
            if let Some(true) = hook.should_stop_after_turn(ctx) {
                return Some(true);
            }
        }
        None
    }

    async fn prepare_next_turn(
        &self,
        ctx: &HookContext,
        assistant: &AssistantMessage,
        tool_results: &[ToolResultMessage],
    ) -> Result<Vec<Message>, HookError> {
        let mut messages = Vec::new();
        for (_id, hook) in &self.hooks {
            match hook.prepare_next_turn(ctx, assistant, tool_results).await {
                Ok(msgs) => messages.extend(msgs),
                Err(_) => return Ok(messages),
            }
        }
        Ok(messages)
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_merged;
#[cfg(test)]
mod tests_merged_stop;
