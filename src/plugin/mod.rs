//! 插件机制（Plugin Registry + 异步工具实例化）。
//!
//! 在 011 [`DeferredToolSpec`]（owned schema 元数据）之上，引入**可失败、可异步
//! 实例化**的工具插件抽象：
//! - [`Plugin`]：声明唯一 `id` + 贡献的工具 schema 集合 + 异步可失败的执行体实例化。
//! - [`PluginTool`]（见 [`tool`] 子模块）：实现 [`Tool`] 的异步惰性包装器——schema
//!   常驻、执行体首次 `execute` 才异步构建并缓存，失败**不缓存、可重试**。
//! - [`PluginRegistry`]：进程内注册表，register/unregister/get/list + 组装全插件
//!   工具为 `Vec<Arc<dyn Tool>>`（可直接喂给 `AgentRuntime.tools`）。
//!
//! 零破坏：产出物仍是合法 [`Tool`]，仍入 `Vec<Arc<dyn Tool>>`，不改 003 主循环
//! 与 `AgentRuntime.tools` 注册契约。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use thiserror::Error;

use crate::core::tool::Tool;
use crate::tools::DeferredToolSpec;

pub mod tool;

pub use tool::PluginTool;

/// 工具插件：声明唯一 `id`、贡献的工具 schema 集合、异步可失败的执行体实例化。
///
/// `tools()` 同步返回 owned schema，注册/组装时即用，**绝不触发实例化**；
/// `instantiate(name)` 的 `name` 必须命中 `tools()` 中某个 spec 的 `name`，
/// 否则应返回 [`PluginError::ToolNotDeclared`]。
#[async_trait]
pub trait Plugin: Send + Sync {
    /// 稳定唯一标识（注册表主键）。
    fn id(&self) -> &str;

    /// 贡献的工具 schema 元数据。注册/组装时即用，**绝不触发实例化**。
    fn tools(&self) -> Vec<DeferredToolSpec>;

    /// 异步实例化某工具执行体（可失败）。惰性：仅首次 `execute` 时被调用。
    async fn instantiate(&self, name: &str) -> Result<Arc<dyn Tool>, PluginError>;
}

/// 插件机制错误。
#[derive(Debug, Error)]
pub enum PluginError {
    /// 注册表已存在同 `id` 插件。
    #[error("plugin already registered: {0}")]
    DuplicatePlugin(String),
    /// 按 `id` 查找插件不存在。
    #[error("plugin not found: {0}")]
    PluginNotFound(String),
    /// `instantiate` 的 `name` 未命中 `tools()` 中任何 spec。
    #[error("tool `{0}` not declared by plugin")]
    ToolNotDeclared(String),
    /// 插件实例化工具执行体失败（携带底层错误源，便于链式诊断）。
    #[error("plugin `{plugin}` failed to instantiate tool `{tool}`: {source}")]
    Instantiation {
        plugin: String,
        tool: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// 进程内插件注册表。
///
/// 用 `std::sync::RwLock`（非 tokio Mutex）：`register/unregister/get/list/tools`
/// 全同步、无 await、短临界区，用 std 锁更轻且避免误跨 await。`instantiate` 的
/// async 逻辑在 [`PluginTool::execute`] 内部，**不经过注册表锁**。
pub struct PluginRegistry {
    plugins: RwLock<HashMap<String, Arc<dyn Plugin>>>,
}

impl PluginRegistry {
    /// 空注册表。
    pub fn new() -> Self {
        Self {
            plugins: RwLock::new(HashMap::new()),
        }
    }

    /// 注册插件；`id` 已存在 → [`PluginError::DuplicatePlugin`]。
    pub fn register(&self, plugin: Arc<dyn Plugin>) -> Result<(), PluginError> {
        let id = plugin.id().to_string();
        let mut guard = self.plugins.write().unwrap_or_else(|e| e.into_inner());
        if guard.contains_key(&id) {
            return Err(PluginError::DuplicatePlugin(id));
        }
        guard.insert(id, plugin);
        Ok(())
    }

    /// 卸载插件，返回被移除的插件（不存在 → `None`）。
    ///
    /// 只阻止新注册/新组装引用该插件；已分发出去的 [`PluginTool`] 持有
    /// `Arc<dyn Plugin>`，仍可正常实例化执行（Arc 延长生命周期）。
    pub fn unregister(&self, id: &str) -> Option<Arc<dyn Plugin>> {
        self.plugins
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id)
    }

    /// 按 `id` 取插件（不存在 → `None`）。
    pub fn get(&self, id: &str) -> Option<Arc<dyn Plugin>> {
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

    /// 组装所有插件的所有工具为 `Vec<Arc<dyn Tool>>`（每个 spec 包一个 [`PluginTool`]）。
    ///
    /// 顺序**确定**：插件按 `id` 字典序，插件内按 `tools()` 声明顺序。
    /// 组装只读 schema，**绝不触发实例化**。
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let guard = self.plugins.read().unwrap_or_else(|e| e.into_inner());
        let mut ids: Vec<&String> = guard.keys().collect();
        ids.sort();
        let mut out: Vec<Arc<dyn Tool>> = Vec::new();
        for id in ids {
            let plugin = &guard[id];
            for spec in plugin.tools() {
                out.push(Arc::new(PluginTool::new(Arc::clone(plugin), spec)));
            }
        }
        out
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::core::tool::ResourceScope;
    use crate::tools::EchoTool;
    use tokio_util::sync::CancellationToken;

    /// 构造一个 spec。
    fn spec(name: &str, scope: ResourceScope) -> DeferredToolSpec {
        DeferredToolSpec {
            name: name.to_string(),
            description: format!("desc of {name}"),
            parameters: Some(serde_json::json!({ "type": "object" })),
            resource_scope: scope,
        }
    }

    /// 计数型 fake plugin：记录 `instantiate` 调用次数。
    struct CountingPlugin {
        id: String,
        specs: Vec<DeferredToolSpec>,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Plugin for CountingPlugin {
        fn id(&self) -> &str {
            &self.id
        }
        fn tools(&self) -> Vec<DeferredToolSpec> {
            self.specs.clone()
        }
        async fn instantiate(&self, name: &str) -> Result<Arc<dyn Tool>, PluginError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.specs.iter().any(|s| s.name == name) {
                Ok(Arc::new(EchoTool))
            } else {
                Err(PluginError::ToolNotDeclared(name.to_string()))
            }
        }
    }

    /// 便捷构造：返回 `(Arc<dyn Plugin>, 计数 Arc)`。
    fn counting(id: &str, names: &[&str]) -> (Arc<dyn Plugin>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let specs = names
            .iter()
            .map(|n| spec(n, ResourceScope::ReadOnly))
            .collect();
        let plugin: Arc<dyn Plugin> = Arc::new(CountingPlugin {
            id: id.to_string(),
            specs,
            calls: calls.clone(),
        });
        (plugin, calls)
    }

    /// register 重复 id → DuplicatePlugin。
    #[test]
    fn test_register_duplicate_id() {
        let registry = PluginRegistry::new();
        let (p1, _) = counting("p", &[]);
        let (p2, _) = counting("p", &[]);
        registry.register(Arc::clone(&p1)).expect("first register");
        let result = registry.register(Arc::clone(&p2));
        assert!(
            matches!(&result, Err(PluginError::DuplicatePlugin(id)) if id == "p"),
            "expected DuplicatePlugin, got {result:?}"
        );
    }

    /// unregister 不存在 → None。
    #[test]
    fn test_unregister_nonexistent() {
        let registry = PluginRegistry::new();
        assert!(registry.unregister("nope").is_none());
    }

    /// get/list 正确，list 按 id 字典序。
    #[test]
    fn test_get_and_list_sorted() {
        let registry = PluginRegistry::new();
        let (b, _) = counting("b", &[]);
        let (a, _) = counting("a", &[]);
        let (c, _) = counting("c", &[]);
        registry.register(Arc::clone(&b)).expect("register b");
        registry.register(Arc::clone(&a)).expect("register a");
        registry.register(Arc::clone(&c)).expect("register c");
        assert!(registry.get("a").is_some());
        assert!(registry.get("b").is_some());
        assert!(registry.get("c").is_some());
        assert!(registry.get("missing").is_none());
        assert_eq!(
            registry.list(),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    /// tools() 组装：多插件多工具，顺序 = 插件 id 字典序 × 声明序，未触发实例化。
    #[test]
    fn test_tools_assembly_order_and_no_instantiation() {
        let registry = PluginRegistry::new();
        let (b, calls_b) = counting("b", &["b1", "b2"]);
        let (a, calls_a) = counting("a", &["a1", "a2"]);
        registry.register(Arc::clone(&b)).expect("register b");
        registry.register(Arc::clone(&a)).expect("register a");
        let tools = registry.tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(names, vec!["a1", "a2", "b1", "b2"]);
        // schema 可读
        assert_eq!(tools[0].description(), "desc of a1");
        assert_eq!(tools[0].resource_scope(), ResourceScope::ReadOnly);
        assert!(tools[0].parameters().is_some());
        // 未触发实例化
        assert_eq!(
            calls_a.load(Ordering::SeqCst),
            0,
            "assembly must not instantiate"
        );
        assert_eq!(
            calls_b.load(Ordering::SeqCst),
            0,
            "assembly must not instantiate"
        );
    }

    /// unregister 语义：组装出的 PluginTool 在 unregister 后仍可 execute 成功。
    #[tokio::test]
    async fn test_unregistered_plugin_tool_still_executes() {
        let registry = PluginRegistry::new();
        let (p, _) = counting("p", &["echo"]);
        registry.register(Arc::clone(&p)).expect("register");
        let tools = registry.tools();
        assert_eq!(tools.len(), 1);
        let removed = registry.unregister("p");
        assert!(removed.is_some(), "unregister should return the plugin");
        assert!(registry.get("p").is_none());
        // 已分发的 PluginTool 仍可 execute（Arc 延长生命周期）
        let result = tools[0]
            .execute(
                "c1",
                serde_json::json!({ "message": "hi" }),
                CancellationToken::new(),
                None,
            )
            .await
            .expect("execute after unregister should succeed");
        assert!(!result.is_error);
    }
}
