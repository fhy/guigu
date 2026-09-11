//! Task 029 集成测试：Agent 插件 / 生命周期钩子走完整主循环桥接。
//!
//! 覆盖验收分支：
//! - 注册 / 合并 hooks / 工厂（AgentPluginRegistry 端到端）
//! - 桥接：`Arc<dyn LifecycleHooks>` 注入 `LoopConfig` 后，主循环在工具执行前后
//!   实际调用钩子（fake hook 计数断言）
//! - 插件钩子先执行、闭包钩子后执行（共存时顺序断言）
//! - `after_tool_call` 改写：插件改写后的 result 再交闭包二次改写
//! - `prepare_next_turn` 注入：插件注入消息在前、闭包注入消息在后
//! - `should_stop_after_turn`：插件 `Some(true)` 即停
//! - 既有闭包钩子行为不回归（无插件钩子时闭包钩子照常工作）
//!
//! fake provider / tool / hooks 用内存计数，不依赖外部服务或硬编码路径。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use guigu::core::message::{
    AssistantContent, AssistantMessage, Message, StopReason, ThinkingLevel, ToolCall, UserContent,
    UserMessage,
};
use guigu::core::provider::{
    AssistantEvent, AssistantStream, ModelProvider, ProviderError, ProviderRequest,
};
use guigu::core::tool::{ResourceScope, Tool, ToolError, ToolResult};
use guigu::core::{
    Agent, AgentConfig, AgentHandle, AgentRuntime, LoopConfig, Model, ToolExecutionMode,
};
use guigu::{AgentPlugin, AgentPluginRegistry, HookContext, HookError, LifecycleHooks};
use tokio_util::sync::CancellationToken;

// ---------- Fake provider ----------

/// 脚本化 provider：按 turn 顺序回放 `AssistantEvent`。
struct FakeProvider {
    turns: Vec<Vec<AssistantEvent>>,
    call_index: AtomicUsize,
    call_count: AtomicUsize,
}

impl FakeProvider {
    fn new(turns: Vec<Vec<AssistantEvent>>) -> Arc<Self> {
        Arc::new(FakeProvider {
            turns,
            call_index: AtomicUsize::new(0),
            call_count: AtomicUsize::new(0),
        })
    }

    fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ModelProvider for FakeProvider {
    async fn stream(&self, _request: ProviderRequest) -> Result<AssistantStream, ProviderError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        let idx = self.call_index.fetch_add(1, Ordering::SeqCst);
        let events = self.turns.get(idx).cloned().unwrap_or_default();
        Ok(Box::pin(stream::iter(events)))
    }
}

// ---------- Fake tool ----------

/// 记录型 fake tool：返回确定性结果。
struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "echo fake tool"
    }
    fn resource_scope(&self) -> ResourceScope {
        ResourceScope::ReadOnly
    }
    async fn execute(
        &self,
        _id: &str,
        _args: serde_json::Value,
        _signal: CancellationToken,
        _on_update: Option<&(dyn Fn(ToolResult) + Send + Sync)>,
    ) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::text("echo-result"))
    }
}

// ---------- Fake hooks ----------

/// 计数型 fake hooks：记录各钩子调用次数，可配置改写 / 注入 / 停止行为。
struct CountingHooks {
    before_calls: Arc<AtomicUsize>,
    after_calls: Arc<AtomicUsize>,
    stop_calls: Arc<AtomicUsize>,
    prepare_calls: Arc<AtomicUsize>,
    /// after_tool_call 改写前缀（None = 不改写，原样透传）。
    after_prefix: Option<String>,
    /// prepare_next_turn 注入消息文本（None = 不注入）。
    inject_text: Option<String>,
    /// should_stop_after_turn 返回值（None = 不干预）。
    stop_value: Option<bool>,
}

impl CountingHooks {
    fn new() -> Self {
        Self {
            before_calls: Arc::new(AtomicUsize::new(0)),
            after_calls: Arc::new(AtomicUsize::new(0)),
            stop_calls: Arc::new(AtomicUsize::new(0)),
            prepare_calls: Arc::new(AtomicUsize::new(0)),
            after_prefix: None,
            inject_text: None,
            stop_value: None,
        }
    }

    fn with_after_prefix(prefix: &str) -> Self {
        let mut h = Self::new();
        h.after_prefix = Some(prefix.to_string());
        h
    }

    fn with_inject(text: &str) -> Self {
        let mut h = Self::new();
        h.inject_text = Some(text.to_string());
        h
    }

    fn with_stop(value: bool) -> Self {
        let mut h = Self::new();
        h.stop_value = Some(value);
        h
    }
}

#[async_trait]
impl LifecycleHooks for CountingHooks {
    async fn before_tool_call(
        &self,
        _ctx: &HookContext,
        _args: &serde_json::Value,
    ) -> Result<(), HookError> {
        self.before_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn after_tool_call(
        &self,
        _ctx: &HookContext,
        _tool_call: &ToolCall,
        result: ToolResult,
    ) -> Result<ToolResult, HookError> {
        self.after_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(prefix) = &self.after_prefix {
            let text = match &result.content[0] {
                guigu::core::message::ToolResultContent::Text { text } => text.clone(),
                _ => String::new(),
            };
            return Ok(ToolResult {
                content: vec![guigu::core::message::ToolResultContent::Text {
                    text: format!("{prefix}:{text}"),
                }],
                is_error: result.is_error,
                details: None,
            });
        }
        Ok(result)
    }

    fn should_stop_after_turn(&self, _ctx: &HookContext) -> Option<bool> {
        self.stop_calls.fetch_add(1, Ordering::SeqCst);
        self.stop_value
    }

    async fn prepare_next_turn(
        &self,
        _ctx: &HookContext,
        _assistant: &AssistantMessage,
        _tool_results: &[guigu::core::message::ToolResultMessage],
    ) -> Result<Vec<Message>, HookError> {
        self.prepare_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(text) = &self.inject_text {
            return Ok(vec![Message::User(UserMessage {
                content: vec![UserContent::Text { text: text.clone() }],
                timestamp: 0,
            })]);
        }
        Ok(Vec::new())
    }
}

// ---------- Fake agent plugin ----------

/// fake agent plugin：贡献 hooks。
struct HooksPlugin {
    id: String,
    hooks: Arc<dyn LifecycleHooks>,
}

impl AgentPlugin for HooksPlugin {
    fn id(&self) -> &str {
        &self.id
    }
    fn hooks(&self) -> Option<Arc<dyn LifecycleHooks>> {
        Some(Arc::clone(&self.hooks))
    }
    fn agent_factory(&self) -> Option<Arc<dyn guigu::AgentFactory>> {
        None
    }
}

// ---------- 脚本与配置 ----------

/// 将 `Arc<CountingHooks>` 转换为 `Arc<dyn LifecycleHooks>`（触发 unsized coercion）。
fn as_hooks(hooks: Arc<CountingHooks>) -> Arc<dyn LifecycleHooks> {
    hooks
}

fn text_turn(text: &str) -> Vec<AssistantEvent> {
    let message = AssistantMessage {
        content: vec![AssistantContent::Text {
            text: text.to_string(),
        }],
        model: None,
        usage: None,
        stop_reason: Some(StopReason::Completed),
        error_message: None,
        timestamp: 0,
    };
    vec![
        AssistantEvent::TextDelta {
            text: text.to_string(),
        },
        AssistantEvent::Done { message },
    ]
}

fn tool_call_turn(id: &str, name: &str, args: &str) -> Vec<AssistantEvent> {
    let message = AssistantMessage {
        content: vec![AssistantContent::ToolCall(ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: args.to_string(),
        })],
        model: None,
        usage: None,
        stop_reason: Some(StopReason::Completed),
        error_message: None,
        timestamp: 0,
    };
    vec![
        AssistantEvent::ToolCallStart {
            id: id.to_string(),
            name: name.to_string(),
            arguments: args.to_string(),
        },
        AssistantEvent::ToolCallEnd { id: id.to_string() },
        AssistantEvent::Done { message },
    ]
}

fn make_config() -> AgentConfig {
    AgentConfig {
        system_prompt: "test".to_string(),
        model: Some("test-model".to_string()),
        thinking_level: ThinkingLevel::Off,
    }
}

fn make_runtime(
    provider: Arc<FakeProvider>,
    tools: Vec<Arc<dyn Tool>>,
    hooks: Option<Arc<dyn LifecycleHooks>>,
) -> AgentRuntime {
    AgentRuntime {
        provider,
        tools,
        loop_config: LoopConfig {
            model: Model {
                id: "test-model".to_string(),
                context_window: 8192,
            },
            tool_execution: ToolExecutionMode::Sequential,
            retry_base_delay: Duration::from_millis(1),
            hooks,
            ..LoopConfig::default()
        },
    }
}

fn user_msg(text: &str) -> Message {
    Message::User(UserMessage {
        content: vec![UserContent::Text {
            text: text.to_string(),
        }],
        timestamp: 0,
    })
}

/// 从 transcript 提取所有 ToolResult 的文本内容（按顺序）。
fn tool_result_texts(messages: &[Arc<Message>]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|m| match m.as_ref() {
            Message::ToolResult(tr) => tr.content.iter().find_map(|c| match c {
                guigu::core::message::ToolResultContent::Text { text } => Some(text.clone()),
                _ => None,
            }),
            _ => None,
        })
        .collect()
}

/// 从 transcript 提取所有 User 消息的文本内容（按顺序）。
fn user_msg_texts(messages: &[Arc<Message>]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|m| match m.as_ref() {
            Message::User(u) => u.content.iter().find_map(|c| match c {
                UserContent::Text { text } => Some(text.clone()),
                _ => None,
            }),
            _ => None,
        })
        .collect()
}

// ---------- 测试 ----------

/// 桥接：before_tool_call + after_tool_call 在主循环中实际被调用（计数断言）。
#[tokio::test]
async fn test_bridge_before_after_tool_call() {
    let provider = FakeProvider::new(vec![tool_call_turn("c1", "echo", "{}"), text_turn("done")]);
    let counting = Arc::new(CountingHooks::new());
    let hooks = as_hooks(Arc::clone(&counting));
    let tools = vec![Arc::new(EchoTool) as Arc<dyn Tool>];
    let handle = AgentHandle::spawn(
        make_config(),
        make_runtime(provider.clone(), tools, Some(Arc::clone(&hooks))),
    );
    handle
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");
    handle.wait_for_idle().await.expect("should settle");

    assert_eq!(provider.call_count(), 2, "two turns");
    assert_eq!(
        counting.before_calls.load(Ordering::SeqCst),
        1,
        "before_tool_call should be called once"
    );
    assert_eq!(
        counting.after_calls.load(Ordering::SeqCst),
        1,
        "after_tool_call should be called once"
    );
}

/// 桥接：after_tool_call 改写——插件改写后的 result 进入 transcript。
#[tokio::test]
async fn test_bridge_after_tool_call_rewrite() {
    let provider = FakeProvider::new(vec![tool_call_turn("c1", "echo", "{}"), text_turn("done")]);
    let hooks: Arc<dyn LifecycleHooks> = Arc::new(CountingHooks::with_after_prefix("plugin"));
    let tools = vec![Arc::new(EchoTool) as Arc<dyn Tool>];
    let handle = AgentHandle::spawn(
        make_config(),
        make_runtime(provider.clone(), tools, Some(Arc::clone(&hooks))),
    );
    handle
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");
    handle.wait_for_idle().await.expect("should settle");

    let snapshot = handle.snapshot();
    let texts = tool_result_texts(&snapshot.messages);
    assert_eq!(texts.len(), 1, "one tool result");
    assert_eq!(
        texts[0], "plugin:echo-result",
        "after_tool_call should rewrite the result"
    );
}

/// 桥接：prepare_next_turn 注入——插件注入的消息进入 transcript。
#[tokio::test]
async fn test_bridge_prepare_next_turn_inject() {
    let provider = FakeProvider::new(vec![tool_call_turn("c1", "echo", "{}"), text_turn("done")]);
    let hooks: Arc<dyn LifecycleHooks> = Arc::new(CountingHooks::with_inject("injected-by-plugin"));
    let tools = vec![Arc::new(EchoTool) as Arc<dyn Tool>];
    let handle = AgentHandle::spawn(
        make_config(),
        make_runtime(provider.clone(), tools, Some(Arc::clone(&hooks))),
    );
    handle
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");
    handle.wait_for_idle().await.expect("should settle");

    let snapshot = handle.snapshot();
    let user_texts = user_msg_texts(&snapshot.messages);
    assert!(
        user_texts.contains(&"injected-by-plugin".to_string()),
        "prepare_next_turn should inject the message, got {user_texts:?}"
    );
}

/// 桥接：should_stop_after_turn 插件 Some(true) 即停——工具 turn 后不再继续。
#[tokio::test]
async fn test_bridge_should_stop() {
    let provider = FakeProvider::new(vec![
        tool_call_turn("c1", "echo", "{}"),
        text_turn("should-not-reach"),
    ]);
    let counting = Arc::new(CountingHooks::with_stop(true));
    let hooks = as_hooks(Arc::clone(&counting));
    let tools = vec![Arc::new(EchoTool) as Arc<dyn Tool>];
    let handle = AgentHandle::spawn(
        make_config(),
        make_runtime(provider.clone(), tools, Some(Arc::clone(&hooks))),
    );
    handle
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");
    handle.wait_for_idle().await.expect("should settle");

    // should_stop_after_turn 返回 Some(true) → 工具 turn 后停止，不再调用 provider。
    assert_eq!(
        provider.call_count(),
        1,
        "should stop after tool turn, not call provider again"
    );
    assert_eq!(
        counting.stop_calls.load(Ordering::SeqCst),
        1,
        "should_stop_after_turn should be called once"
    );
}

/// 桥接：无插件钩子时闭包钩子照常工作（不回归）。
#[tokio::test]
async fn test_bridge_closure_hooks_no_regression() {
    let provider = FakeProvider::new(vec![tool_call_turn("c1", "echo", "{}"), text_turn("done")]);
    let tools = vec![Arc::new(EchoTool) as Arc<dyn Tool>];
    // 仅设置闭包钩子（无插件钩子）。
    let closure_after_calls = Arc::new(AtomicUsize::new(0));
    let closure_after = closure_after_calls.clone();
    let handle = AgentHandle::spawn(
        make_config(),
        AgentRuntime {
            provider: provider.clone(),
            tools,
            loop_config: LoopConfig {
                model: Model {
                    id: "test-model".to_string(),
                    context_window: 8192,
                },
                tool_execution: ToolExecutionMode::Sequential,
                retry_base_delay: Duration::from_millis(1),
                after_tool_call: Some(Box::new(move |_tc: &ToolCall, r: ToolResult| {
                    closure_after.fetch_add(1, Ordering::SeqCst);
                    r
                })),
                ..LoopConfig::default()
            },
        },
    );
    handle
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");
    handle.wait_for_idle().await.expect("should settle");

    assert_eq!(provider.call_count(), 2, "two turns");
    assert_eq!(
        closure_after_calls.load(Ordering::SeqCst),
        1,
        "closure after_tool_call should be called once (no regression)"
    );
}

/// 桥接：插件钩子先执行、闭包钩子后执行——after_tool_call 插件改写后闭包二次改写。
#[tokio::test]
async fn test_bridge_plugin_before_closure() {
    let provider = FakeProvider::new(vec![tool_call_turn("c1", "echo", "{}"), text_turn("done")]);
    let tools = vec![Arc::new(EchoTool) as Arc<dyn Tool>];
    // 插件钩子：前缀 "plugin"。闭包钩子：前缀 "closure"。
    // 期望最终结果："closure:plugin:echo-result"（插件先、闭包后）。
    let hooks: Arc<dyn LifecycleHooks> = Arc::new(CountingHooks::with_after_prefix("plugin"));
    let handle = AgentHandle::spawn(
        make_config(),
        AgentRuntime {
            provider: provider.clone(),
            tools,
            loop_config: LoopConfig {
                model: Model {
                    id: "test-model".to_string(),
                    context_window: 8192,
                },
                tool_execution: ToolExecutionMode::Sequential,
                retry_base_delay: Duration::from_millis(1),
                after_tool_call: Some(Box::new(move |_tc: &ToolCall, r: ToolResult| {
                    let text = match &r.content[0] {
                        guigu::core::message::ToolResultContent::Text { text } => text.clone(),
                        _ => String::new(),
                    };
                    ToolResult {
                        content: vec![guigu::core::message::ToolResultContent::Text {
                            text: format!("closure:{text}"),
                        }],
                        is_error: r.is_error,
                        details: None,
                    }
                })),
                hooks: Some(Arc::clone(&hooks)),
                ..LoopConfig::default()
            },
        },
    );
    handle
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");
    handle.wait_for_idle().await.expect("should settle");

    let snapshot = handle.snapshot();
    let texts = tool_result_texts(&snapshot.messages);
    assert_eq!(texts.len(), 1, "one tool result");
    assert_eq!(
        texts[0], "closure:plugin:echo-result",
        "plugin hook should execute first, then closure hook"
    );
}

/// 注册 / 合并 hooks / 工厂：AgentPluginRegistry 端到端。
#[tokio::test]
async fn test_registry_end_to_end() {
    let registry = AgentPluginRegistry::new();
    let counting_a = Arc::new(CountingHooks::with_after_prefix("a"));
    let counting_b = Arc::new(CountingHooks::with_after_prefix("b"));
    let hooks_a = as_hooks(Arc::clone(&counting_a));
    let hooks_b = as_hooks(Arc::clone(&counting_b));
    registry
        .register(Arc::new(HooksPlugin {
            id: "a".to_string(),
            hooks: Arc::clone(&hooks_a),
        }))
        .expect("register a");
    registry
        .register(Arc::new(HooksPlugin {
            id: "b".to_string(),
            hooks: Arc::clone(&hooks_b),
        }))
        .expect("register b");

    // list 字典序。
    assert_eq!(registry.list(), vec!["a".to_string(), "b".to_string()]);

    // merged_hooks 返回 Some。
    let merged = registry.merged_hooks().expect("should have merged hooks");

    // 验证合并后的 after_tool_call 按 id 字典序按值串接：a 前缀 → b 前缀。
    let tool_call = ToolCall {
        id: "c1".to_string(),
        name: "echo".to_string(),
        arguments: "{}".to_string(),
    };
    let result = ToolResult::text("orig");
    let out = merged
        .after_tool_call(&HookContext::new(&[]), &tool_call, result)
        .await
        .expect("should be Ok");
    let text = match &out.content[0] {
        guigu::core::message::ToolResultContent::Text { text } => text.clone(),
        _ => unreachable!(),
    };
    assert_eq!(
        text, "b:a:orig",
        "merged after_tool_call should chain by id order: a then b"
    );
    assert_eq!(
        counting_a.after_calls.load(Ordering::SeqCst),
        1,
        "a's after_tool_call should be called"
    );
    assert_eq!(
        counting_b.after_calls.load(Ordering::SeqCst),
        1,
        "b's after_tool_call should be called"
    );
}

/// 注册重复 id → DuplicateAgentPlugin。
#[test]
fn test_registry_duplicate_id() {
    let registry = AgentPluginRegistry::new();
    let hooks = as_hooks(Arc::new(CountingHooks::new()));
    registry
        .register(Arc::new(HooksPlugin {
            id: "p".to_string(),
            hooks: Arc::clone(&hooks),
        }))
        .expect("first register");
    let result = registry.register(Arc::new(HooksPlugin {
        id: "p".to_string(),
        hooks: Arc::clone(&hooks),
    }));
    assert!(
        matches!(
            &result,
            Err(guigu::AgentPluginError::DuplicateAgentPlugin(id)) if id == "p"
        ),
        "expected DuplicateAgentPlugin, got {result:?}"
    );
}

/// 无插件贡献 hooks 时 merged_hooks 返回 None。
#[test]
fn test_registry_merged_hooks_none() {
    let registry = AgentPluginRegistry::new();
    // 注册一个不贡献 hooks 的插件。
    struct NoHooksPlugin;
    impl AgentPlugin for NoHooksPlugin {
        fn id(&self) -> &str {
            "no-hooks"
        }
        fn hooks(&self) -> Option<Arc<dyn LifecycleHooks>> {
            None
        }
        fn agent_factory(&self) -> Option<Arc<dyn guigu::AgentFactory>> {
            None
        }
    }
    registry
        .register(Arc::new(NoHooksPlugin))
        .expect("register");
    assert!(registry.merged_hooks().is_none());
}

/// unregister 语义：已取出的 Arc<dyn AgentPlugin> 在 unregister 后仍可调用。
#[test]
fn test_registry_unregister_still_callable() {
    let registry = AgentPluginRegistry::new();
    let hooks = as_hooks(Arc::new(CountingHooks::new()));
    let plugin: Arc<dyn AgentPlugin> = Arc::new(HooksPlugin {
        id: "p".to_string(),
        hooks: Arc::clone(&hooks),
    });
    registry.register(Arc::clone(&plugin)).expect("register");
    let removed = registry.unregister("p");
    assert!(removed.is_some(), "unregister should return the plugin");
    assert!(registry.get("p").is_none());
    // 已取出的 Arc 仍可调用（Arc 延长生命周期）。
    assert!(plugin.hooks().is_some());
    assert_eq!(plugin.id(), "p");
}

/// 组合 hooks 的调用发生在注册表锁外：fake plugin 在 hook 内重新进入 registry
/// 不产生死锁（对齐 016 r1 教训）。
#[tokio::test]
async fn test_registry_merged_hooks_outside_lock() {
    let registry = Arc::new(AgentPluginRegistry::new());
    // 创建一个在 hook 内重入 registry 的 hooks。
    struct ReentrantHooks {
        registry: Arc<AgentPluginRegistry>,
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl LifecycleHooks for ReentrantHooks {
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
    let calls = Arc::new(AtomicUsize::new(0));
    registry
        .register(Arc::new(HooksPlugin {
            id: "probe".to_string(),
            hooks: Arc::new(ReentrantHooks {
                registry: Arc::clone(&registry),
                calls: calls.clone(),
            }),
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
