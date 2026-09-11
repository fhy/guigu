//! Agent 层插件：自定义 agent 类型工厂 + 生命周期钩子贡献（Task 029）。
//!
//! - [`AgentFactory`]：按 id 构建一个 [`Agent`](crate::core::agent::Agent) 实例。
//! - [`AgentPlugin`]：`id` + 贡献 [`LifecycleHooks`](crate::plugin::hooks::LifecycleHooks)
//!   + 可选贡献 [`AgentFactory`]。独立新 trait，不改 016 [`Plugin`](crate::plugin::Plugin)。
//! - [`AgentPluginRegistry`]：进程内注册表，register / unregister / get / list /
//!   合并 hooks / 按 id 取工厂。
//!
//! 零破坏：不改 001 [`Agent`](crate::core::agent::Agent) trait、不改 016
//! [`Plugin`](crate::plugin::Plugin) trait。Agent 插件为独立并行机制，不与工具插件耦合。
//!
//! 模块拆分（单文件 ≤ 400 行约束）：单测在 `agent/tests.rs`。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use thiserror::Error;

use crate::core::agent::{Agent, AgentConfig};
use crate::plugin::hooks::{LifecycleHooks, MergedHooks};

/// Agent 层插件错误。
#[derive(Debug, Error)]
pub enum AgentPluginError {
    /// 注册表已存在同 `id` 插件。
    #[error("agent plugin already registered: {0}")]
    DuplicateAgentPlugin(String),
    /// 按 `id` 查找插件不存在。
    #[error("agent plugin not found: {0}")]
    AgentPluginNotFound(String),
    /// `AgentFactory::build` 构建 agent 失败。
    #[error("agent factory build failed: {0}")]
    BuildFailed(String),
}

/// 自定义 agent 类型工厂：按 id 构建一个 Agent 实例。
#[async_trait]
pub trait AgentFactory: Send + Sync {
    /// 稳定唯一标识。
    fn id(&self) -> &str;

    /// 构建 agent。`config` 为 001 定稿的 [`AgentConfig`]，`hooks` 为合并后的
    /// 生命周期钩子。产出 [`Arc<dyn Agent>`]（001 定稿 trait 对象），不改 001 签名。
    async fn build(
        &self,
        config: AgentConfig,
        hooks: Arc<dyn LifecycleHooks>,
    ) -> Result<Arc<dyn Agent>, AgentPluginError>;
}

/// Agent 层插件：贡献生命周期钩子 + 可选的自定义 agent 工厂。
///
/// 独立新 trait（不改 016 [`Plugin`](crate::plugin::Plugin)）：一个插件可同时实现
/// `Plugin`（工具）+ `AgentPlugin`（Agent 层），但两个 trait 各自独立注册、互不耦合。
pub trait AgentPlugin: Send + Sync {
    /// 稳定唯一标识（注册表主键）。
    fn id(&self) -> &str;

    /// 贡献的生命周期钩子；`None` = 不贡献。
    fn hooks(&self) -> Option<Arc<dyn LifecycleHooks>>;

    /// 贡献的自定义 agent 工厂；`None` = 不贡献。
    fn agent_factory(&self) -> Option<Arc<dyn AgentFactory>>;
}

/// 进程内 Agent 插件注册表。
///
/// 用 `std::sync::RwLock`（同 016，短临界区无 await）。`merged_hooks` 返回的
/// 组合钩子**在锁外**调用各插件 hook（只复制 `Arc<dyn AgentPlugin>` 后释放锁，
/// 避免外部回调置于注册表锁内——对齐 016 r1 教训）。
pub struct AgentPluginRegistry {
    plugins: RwLock<HashMap<String, Arc<dyn AgentPlugin>>>,
}

impl AgentPluginRegistry {
    /// 空注册表。
    pub fn new() -> Self {
        Self {
            plugins: RwLock::new(HashMap::new()),
        }
    }

    /// 注册插件；`id` 已存在 → [`AgentPluginError::DuplicateAgentPlugin`]。
    pub fn register(&self, p: Arc<dyn AgentPlugin>) -> Result<(), AgentPluginError> {
        let id = p.id().to_string();
        let mut guard = self.plugins.write().unwrap_or_else(|e| e.into_inner());
        if guard.contains_key(&id) {
            return Err(AgentPluginError::DuplicateAgentPlugin(id));
        }
        guard.insert(id, p);
        Ok(())
    }

    /// 卸载插件，返回被移除的插件（不存在 → `None`）。
    ///
    /// 只阻止新查询 / 新合并；已分发出去的 `Arc<dyn AgentPlugin>` 仍可调用
    /// （Arc 延长生命周期）。
    pub fn unregister(&self, id: &str) -> Option<Arc<dyn AgentPlugin>> {
        self.plugins
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id)
    }

    /// 按 `id` 取插件（不存在 → `None`）。
    pub fn get(&self, id: &str) -> Option<Arc<dyn AgentPlugin>> {
        self.plugins
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    /// 已注册插件 `id` 列表，**按 `id` 字典序稳定排序**。
    pub fn list(&self) -> Vec<String> {
        let guard = self.plugins.read().unwrap_or_else(|e| e.into_inner());
        let mut ids: Vec<String> = guard.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// 合并所有插件 hooks 为单个 [`LifecycleHooks`]（按 id 字典序链式调用，
    /// 任一失败即短路）。无插件贡献 hooks 时返回 `None`。
    ///
    /// 锁纪律：读锁内**仅**复制 `Arc<dyn AgentPlugin>`，随即释放读锁；
    /// 外部 `plugin.hooks()` 回调在**锁外**调用，回调内可安全重入注册表
    /// （`register` / `unregister` / `get`）而不会阻塞或死锁。
    pub fn merged_hooks(&self) -> Option<Arc<dyn LifecycleHooks>> {
        // 锁内：仅复制 Arc<dyn AgentPlugin>，不调用任何外部回调。
        let plugins: Vec<Arc<dyn AgentPlugin>> = {
            let guard = self.plugins.read().unwrap_or_else(|e| e.into_inner());
            guard.values().cloned().collect()
        };
        // 锁外：遍历副本调用外部 plugin.hooks() 并收集。
        let entries: Vec<(String, Arc<dyn LifecycleHooks>)> = plugins
            .iter()
            .filter_map(|plugin| plugin.hooks().map(|hooks| (plugin.id().to_string(), hooks)))
            .collect();
        if entries.is_empty() {
            None
        } else {
            Some(Arc::new(MergedHooks::new(entries)))
        }
    }

    /// 按 `id` 取 agent 工厂（插件不存在或不贡献工厂 → `None`）。
    ///
    /// 锁纪律：读锁内**仅**查找并复制 `Arc<dyn AgentPlugin>`，随即释放读锁；
    /// 外部 `plugin.agent_factory()` 回调在**锁外**调用，回调内可安全重入注册表
    /// （`register` / `unregister` / `get`）而不会阻塞或死锁。
    pub fn agent_factory(&self, id: &str) -> Option<Arc<dyn AgentFactory>> {
        // 锁内：仅查找并复制 Arc<dyn AgentPlugin>，不调用任何外部回调。
        let plugin: Option<Arc<dyn AgentPlugin>> = {
            let guard = self.plugins.read().unwrap_or_else(|e| e.into_inner());
            guard.get(id).cloned()
        };
        // 锁外：调用外部 plugin.agent_factory() 回调。
        plugin.and_then(|plugin| plugin.agent_factory())
    }
}

impl Default for AgentPluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
