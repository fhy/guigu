//! [`super`] 单测：注册表 / 工厂 / 锁纪律 / unregister 语义。
//!
//! 自 `agent.rs` 拆出（单文件 ≤ 400 行约束，Task 029）。

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::core::agent::{AgentError, AgentSnapshot};
use crate::core::event::AgentEvent;
use crate::core::message::{Message, ThinkingLevel};
use crate::plugin::hooks::{HookContext, HookError};
use tokio::sync::broadcast;

/// 最小 fake agent：实现 Agent trait，snapshot 返回固定值，其余空操作。
struct FakeAgent {
    snapshot: AgentSnapshot,
}

impl FakeAgent {
    fn new() -> Self {
        Self {
            snapshot: AgentSnapshot {
                system_prompt: "fake".to_string(),
                model: Some("fake-model".to_string()),
                thinking_level: ThinkingLevel::Off,
                messages: Vec::new(),
                is_streaming: false,
                streaming_message: None,
                pending_tool_calls: std::collections::HashSet::new(),
                error_message: None,
            },
        }
    }
}

#[async_trait]
impl Agent for FakeAgent {
    fn snapshot(&self) -> AgentSnapshot {
        self.snapshot.clone()
    }
    fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        let (tx, rx) = broadcast::channel(1);
        drop(tx);
        rx
    }
    async fn prompt(&self, _messages: Vec<Message>) -> Result<(), AgentError> {
        Ok(())
    }
    async fn continue_(&self) -> Result<(), AgentError> {
        Ok(())
    }
    async fn steer(&self, _msg: Message) -> Result<(), AgentError> {
        Ok(())
    }
    async fn follow_up(&self, _msg: Message) -> Result<(), AgentError> {
        Ok(())
    }
    async fn reset(&self) -> Result<(), AgentError> {
        Ok(())
    }
    fn abort(&self) {}
    async fn wait_for_idle(&self) -> Result<(), AgentError> {
        Ok(())
    }
}

/// fake agent 工厂：build 产出 FakeAgent。
struct FakeFactory {
    id: String,
    build_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl AgentFactory for FakeFactory {
    fn id(&self) -> &str {
        &self.id
    }
    async fn build(
        &self,
        _config: AgentConfig,
        _hooks: Arc<dyn LifecycleHooks>,
    ) -> Result<Arc<dyn Agent>, AgentPluginError> {
        self.build_calls.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(FakeAgent::new()) as Arc<dyn Agent>)
    }
}

/// 计数型 fake hooks：记录 before_tool_call 调用次数。
struct CountingHooks {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LifecycleHooks for CountingHooks {
    async fn before_tool_call(
        &self,
        _ctx: &HookContext,
        _args: &serde_json::Value,
    ) -> Result<(), HookError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// fake agent plugin：贡献 hooks + 可选工厂。
struct FakeAgentPlugin {
    id: String,
    hooks: Option<Arc<dyn LifecycleHooks>>,
    factory: Option<Arc<dyn AgentFactory>>,
}

impl AgentPlugin for FakeAgentPlugin {
    fn id(&self) -> &str {
        &self.id
    }
    fn hooks(&self) -> Option<Arc<dyn LifecycleHooks>> {
        self.hooks.clone()
    }
    fn agent_factory(&self) -> Option<Arc<dyn AgentFactory>> {
        self.factory.clone()
    }
}

fn make_config() -> AgentConfig {
    AgentConfig {
        system_prompt: "test".to_string(),
        model: Some("test-model".to_string()),
        thinking_level: ThinkingLevel::Off,
    }
}

/// register 重复 id → DuplicateAgentPlugin。
#[test]
fn test_register_duplicate_id() {
    let registry = AgentPluginRegistry::new();
    let p1: Arc<dyn AgentPlugin> = Arc::new(FakeAgentPlugin {
        id: "p".to_string(),
        hooks: None,
        factory: None,
    });
    let p2: Arc<dyn AgentPlugin> = Arc::new(FakeAgentPlugin {
        id: "p".to_string(),
        hooks: None,
        factory: None,
    });
    registry.register(Arc::clone(&p1)).expect("first register");
    let result = registry.register(Arc::clone(&p2));
    assert!(
        matches!(&result, Err(AgentPluginError::DuplicateAgentPlugin(id)) if id == "p"),
        "expected DuplicateAgentPlugin, got {result:?}"
    );
}

/// unregister 不存在 → None。
#[test]
fn test_unregister_nonexistent() {
    let registry = AgentPluginRegistry::new();
    assert!(registry.unregister("nope").is_none());
}

/// get/list 正确，list 按 id 字典序。
#[test]
fn test_get_and_list_sorted() {
    let registry = AgentPluginRegistry::new();
    for id in ["b", "a", "c"] {
        registry
            .register(Arc::new(FakeAgentPlugin {
                id: id.to_string(),
                hooks: None,
                factory: None,
            }))
            .expect("register");
    }
    assert!(registry.get("a").is_some());
    assert!(registry.get("b").is_some());
    assert!(registry.get("c").is_some());
    assert!(registry.get("missing").is_none());
    assert_eq!(
        registry.list(),
        vec!["a".to_string(), "b".to_string(), "c".to_string()]
    );
}

/// merged_hooks：无插件贡献 hooks → None。
#[test]
fn test_merged_hooks_none_when_no_hooks() {
    let registry = AgentPluginRegistry::new();
    registry
        .register(Arc::new(FakeAgentPlugin {
            id: "p".to_string(),
            hooks: None,
            factory: None,
        }))
        .expect("register");
    assert!(registry.merged_hooks().is_none());
}

/// merged_hooks：多插件贡献 hooks → 返回 Some（组合钩子）。
#[test]
fn test_merged_hooks_some_when_hooks_contributed() {
    let registry = AgentPluginRegistry::new();
    for id in ["a", "b"] {
        let calls = Arc::new(AtomicUsize::new(0));
        registry
            .register(Arc::new(FakeAgentPlugin {
                id: id.to_string(),
                hooks: Some(Arc::new(CountingHooks {
                    calls: calls.clone(),
                }) as Arc<dyn LifecycleHooks>),
                factory: None,
            }))
            .expect("register");
    }
    assert!(registry.merged_hooks().is_some());
}

/// agent_factory(id) 正确取到工厂；不存在 → None。
#[test]
fn test_agent_factory_lookup() {
    let registry = AgentPluginRegistry::new();
    let build_calls = Arc::new(AtomicUsize::new(0));
    registry
        .register(Arc::new(FakeAgentPlugin {
            id: "p".to_string(),
            hooks: None,
            factory: Some(Arc::new(FakeFactory {
                id: "p".to_string(),
                build_calls: build_calls.clone(),
            }) as Arc<dyn AgentFactory>),
        }))
        .expect("register");
    assert!(registry.agent_factory("p").is_some());
    assert!(registry.agent_factory("missing").is_none());
}

/// AgentFactory::build 产出可用的 Arc<dyn Agent>（fake agent 断言 snapshot 生命周期）。
#[tokio::test]
async fn test_factory_build_produces_usable_agent() {
    let registry = AgentPluginRegistry::new();
    let build_calls = Arc::new(AtomicUsize::new(0));
    registry
        .register(Arc::new(FakeAgentPlugin {
            id: "p".to_string(),
            hooks: None,
            factory: Some(Arc::new(FakeFactory {
                id: "p".to_string(),
                build_calls: build_calls.clone(),
            }) as Arc<dyn AgentFactory>),
        }))
        .expect("register");
    let factory = registry.agent_factory("p").expect("factory should exist");
    let hooks = registry.merged_hooks().unwrap_or_else(|| {
        Arc::new(CountingHooks {
            calls: Arc::new(AtomicUsize::new(0)),
        }) as Arc<dyn LifecycleHooks>
    });
    let agent = factory
        .build(make_config(), hooks)
        .await
        .expect("build should succeed");
    assert_eq!(build_calls.load(Ordering::SeqCst), 1, "build called once");
    // 断言 snapshot 生命周期：snapshot 返回固定值。
    let snap = agent.snapshot();
    assert_eq!(snap.system_prompt, "fake");
    assert_eq!(snap.model, Some("fake-model".to_string()));
    // 断言 prompt 可调用（空操作）。
    agent
        .prompt(Vec::new())
        .await
        .expect("prompt should succeed");
}

/// 重入探针插件：`agent_factory()` 回调内重入 registry（register/unregister/get）。
struct ReentrantFactoryPlugin {
    id: String,
    registry: Arc<AgentPluginRegistry>,
    factory: Option<Arc<dyn AgentFactory>>,
    reentered: Arc<AtomicBool>,
}

impl AgentPlugin for ReentrantFactoryPlugin {
    fn id(&self) -> &str {
        &self.id
    }
    fn hooks(&self) -> Option<Arc<dyn LifecycleHooks>> {
        None
    }
    fn agent_factory(&self) -> Option<Arc<dyn AgentFactory>> {
        // 回调内重入 registry：若读锁未释放，register/unregister（写锁）将死锁。
        let helper: Arc<dyn AgentPlugin> = Arc::new(FakeAgentPlugin {
            id: "helper".to_string(),
            hooks: None,
            factory: None,
        });
        self.registry.register(helper).expect("reentrant register");
        assert!(self.registry.unregister("helper").is_some());
        assert!(self.registry.get(&self.id).is_some());
        self.reentered.store(true, Ordering::SeqCst);
        self.factory.clone()
    }
}

/// 锁纪律回归：`agent_factory(id)` 回调期间插件可安全重入 registry
/// （register/unregister/get）而不死锁，且工厂正确返回。
#[test]
fn test_agent_factory_callback_runs_outside_lock() {
    let registry = Arc::new(AgentPluginRegistry::new());
    let reentered = Arc::new(AtomicBool::new(false));
    registry
        .register(Arc::new(ReentrantFactoryPlugin {
            id: "probe".to_string(),
            registry: Arc::clone(&registry),
            factory: Some(Arc::new(FakeFactory {
                id: "probe".to_string(),
                build_calls: Arc::new(AtomicUsize::new(0)),
            }) as Arc<dyn AgentFactory>),
            reentered: reentered.clone(),
        }))
        .expect("register");
    // 若读锁未释放，回调内重入 register/unregister（写锁）将死锁。
    let got = registry
        .agent_factory("probe")
        .expect("factory should be returned");
    assert_eq!(got.id(), "probe");
    assert!(
        reentered.load(Ordering::SeqCst),
        "callback should have re-entered the registry"
    );
    // 重入的 register/unregister 已清理，registry 状态正确。
    assert!(registry.get("helper").is_none());
    assert!(registry.get("probe").is_some());
}

/// 组合 hooks 的调用发生在注册表锁外：fake plugin 在 hook 内重新进入 registry
/// 不产生死锁（对齐 016 r1 教训）。
struct LockProbeHooks {
    registry: Arc<AgentPluginRegistry>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LifecycleHooks for LockProbeHooks {
    async fn before_tool_call(
        &self,
        _ctx: &HookContext,
        _args: &serde_json::Value,
    ) -> Result<(), HookError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // 在 hook 内重新进入 registry（get + list），若锁未释放则死锁/阻塞。
        let _ = self.registry.get("probe");
        let _ = self.registry.list();
        Ok(())
    }
}

/// 锁纪律回归：merged_hooks 调用期间，插件 hook 内可安全重入 registry。
#[tokio::test]
async fn test_merged_hooks_callback_runs_outside_lock() {
    let registry = Arc::new(AgentPluginRegistry::new());
    let calls = Arc::new(AtomicUsize::new(0));
    registry
        .register(Arc::new(FakeAgentPlugin {
            id: "probe".to_string(),
            hooks: Some(Arc::new(LockProbeHooks {
                registry: Arc::clone(&registry),
                calls: calls.clone(),
            }) as Arc<dyn LifecycleHooks>),
            factory: None,
        }))
        .expect("register");
    let merged = registry.merged_hooks().expect("should have hooks");
    // 调用 merged hook（内部会重入 registry），若锁未释放则死锁。
    let result = merged
        .before_tool_call(&HookContext::new(&[]), &serde_json::json!({}))
        .await;
    assert!(result.is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// unregister 语义：已取出的 Arc<dyn AgentPlugin> 在 unregister 后仍可调用。
#[test]
fn test_unregistered_plugin_still_callable() {
    let registry = AgentPluginRegistry::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let plugin: Arc<dyn AgentPlugin> = Arc::new(FakeAgentPlugin {
        id: "p".to_string(),
        hooks: Some(Arc::new(CountingHooks {
            calls: calls.clone(),
        }) as Arc<dyn LifecycleHooks>),
        factory: None,
    });
    registry.register(Arc::clone(&plugin)).expect("register");
    let removed = registry.unregister("p");
    assert!(removed.is_some(), "unregister should return the plugin");
    assert!(registry.get("p").is_none());
    // 已取出的 Arc 仍可调用（Arc 延长生命周期）。
    assert!(plugin.hooks().is_some());
    assert_eq!(plugin.id(), "p");
}
