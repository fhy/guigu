//! Task 016 集成测试：Plugin/PluginTool/PluginRegistry 走完整 Tool trait 契约。
//!
//! 以 `Arc<dyn Tool>` 验证：schema 方法不触发实例化、execute 透传 tool_call_id/args
//! 并返回内层结果、多 task 并发 execute 下 instantiate 仅调用一次（OnceCell 保证）、
//! 多插件多工具组装顺序确定、unregister 后已分发 PluginTool 仍可执行、错误路径映射。
//! fake plugin 用内存计数，不依赖外部服务或硬编码路径。

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use guigu::core::message::ToolResultContent;
use guigu::core::tool::{ResourceScope, Tool, ToolError, ToolResult};
use guigu::tools::DeferredToolSpec;
use guigu::{EchoTool, Plugin, PluginError, PluginRegistry, PluginTool};
use tokio_util::sync::CancellationToken;

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

/// 记录型 fake tool：记录收到的 tool_call_id/args，返回确定性结果。
#[derive(Debug)]
struct RecordingTool {
    received: Arc<Mutex<(String, serde_json::Value)>>,
}

#[async_trait]
impl Tool for RecordingTool {
    fn name(&self) -> &str {
        "rec"
    }
    fn description(&self) -> &str {
        "recording fake tool"
    }
    fn parameters(&self) -> Option<serde_json::Value> {
        None
    }
    fn resource_scope(&self) -> ResourceScope {
        ResourceScope::ReadOnly
    }
    async fn execute(
        &self,
        tool_call_id: &str,
        args: serde_json::Value,
        _signal: CancellationToken,
        _on_update: Option<&(dyn Fn(ToolResult) + Send + Sync)>,
    ) -> Result<ToolResult, ToolError> {
        *self.received.lock().unwrap() = (tool_call_id.to_string(), args);
        Ok(ToolResult::text(format!("rec:{tool_call_id}")))
    }
}

/// 记录型 fake plugin：`instantiate` 返回 `RecordingTool`。
struct RecordingPlugin {
    received: Arc<Mutex<(String, serde_json::Value)>>,
}

#[async_trait]
impl Plugin for RecordingPlugin {
    fn id(&self) -> &str {
        "rec-plugin"
    }
    fn tools(&self) -> Vec<DeferredToolSpec> {
        vec![spec("rec", ResourceScope::ReadOnly)]
    }
    async fn instantiate(&self, _name: &str) -> Result<Arc<dyn Tool>, PluginError> {
        Ok(Arc::new(RecordingTool {
            received: Arc::clone(&self.received),
        }))
    }
}

/// 完整 Tool trait 契约：schema 方法不触发实例化，execute 透传并返回内层结果。
#[tokio::test]
async fn test_plugin_tool_full_contract() {
    let received = Arc::new(Mutex::new((String::new(), serde_json::Value::Null)));
    let plugin = Arc::new(RecordingPlugin {
        received: received.clone(),
    });
    let tool: Arc<dyn Tool> = Arc::new(PluginTool::new(
        plugin,
        spec("rec", ResourceScope::ReadOnly),
    ));

    // schema 方法：不触发实例化
    assert_eq!(tool.name(), "rec");
    assert_eq!(tool.description(), "desc of rec");
    assert_eq!(
        tool.parameters(),
        Some(serde_json::json!({ "type": "object" }))
    );
    assert_eq!(tool.resource_scope(), ResourceScope::ReadOnly);

    // execute：透传 tool_call_id/args，返回内层结果
    let args = serde_json::json!({ "a": 1 });
    let result = tool
        .execute("call-7", args.clone(), CancellationToken::new(), None)
        .await
        .expect("execute should succeed");
    let (id, got_args) = received.lock().unwrap().clone();
    assert_eq!(id, "call-7");
    assert_eq!(got_args, args);
    assert_eq!(
        result.content[0],
        ToolResultContent::Text {
            text: "rec:call-7".to_string()
        }
    );
    assert!(!result.is_error);
}

/// 并发只实例化一次：多 task 并发 execute，instantiate 计数仍为 1。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_concurrent_execute_instantiates_once() {
    let calls = Arc::new(AtomicUsize::new(0));
    let plugin = Arc::new(CountingPlugin {
        id: "p".into(),
        specs: vec![spec("echo", ResourceScope::ReadOnly)],
        calls: calls.clone(),
    });
    let tool: Arc<dyn Tool> = Arc::new(PluginTool::new(
        plugin,
        spec("echo", ResourceScope::ReadOnly),
    ));
    let mut handles = Vec::new();
    for i in 0..32 {
        let tool = Arc::clone(&tool);
        handles.push(tokio::spawn(async move {
            tool.execute(
                &format!("c{i}"),
                serde_json::json!({ "message": format!("m{i}") }),
                CancellationToken::new(),
                None,
            )
            .await
        }));
    }
    for r in futures::future::join_all(handles).await {
        r.expect("task should not panic")
            .expect("execute should succeed");
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "instantiate must run exactly once under concurrency"
    );
}

/// 多插件多工具组装：顺序 = 插件 id 字典序 × 声明序，未触发实例化。
#[test]
fn test_registry_tools_assembly_order() {
    let registry = PluginRegistry::new();
    let calls_b = Arc::new(AtomicUsize::new(0));
    let calls_a = Arc::new(AtomicUsize::new(0));
    registry
        .register(Arc::new(CountingPlugin {
            id: "b".into(),
            specs: vec![
                spec("b1", ResourceScope::ReadOnly),
                spec("b2", ResourceScope::ReadOnly),
            ],
            calls: calls_b.clone(),
        }))
        .expect("register b");
    registry
        .register(Arc::new(CountingPlugin {
            id: "a".into(),
            specs: vec![
                spec("a1", ResourceScope::ReadOnly),
                spec("a2", ResourceScope::ReadOnly),
            ],
            calls: calls_a.clone(),
        }))
        .expect("register a");
    let tools = registry.tools();
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert_eq!(names, vec!["a1", "a2", "b1", "b2"]);
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
    let calls = Arc::new(AtomicUsize::new(0));
    registry
        .register(Arc::new(CountingPlugin {
            id: "p".into(),
            specs: vec![spec("echo", ResourceScope::ReadOnly)],
            calls: calls.clone(),
        }))
        .expect("register");
    let tools = registry.tools();
    assert_eq!(tools.len(), 1);
    let removed = registry.unregister("p");
    assert!(removed.is_some(), "unregister should return the plugin");
    assert!(registry.get("p").is_none());
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
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "instantiate once on execute"
    );
}

/// 错误路径：instantiate 对未声明工具名 → ToolNotDeclared，映射为 ToolError。
#[tokio::test]
async fn test_undeclared_tool_maps_to_tool_error() {
    let plugin = Arc::new(CountingPlugin {
        id: "p".into(),
        specs: vec![spec("declared", ResourceScope::ReadOnly)],
        calls: Arc::new(AtomicUsize::new(0)),
    });
    // 直接构造一个 spec name 未声明的 PluginTool
    let tool = PluginTool::new(plugin, spec("undeclared", ResourceScope::ReadOnly));
    let result = tool
        .execute("c1", serde_json::json!({}), CancellationToken::new(), None)
        .await;
    assert!(result.is_err(), "execute should fail for undeclared tool");
    let err = result.unwrap_err();
    assert!(
        err.message.contains("not declared"),
        "error should mention not declared: {}",
        err.message
    );
}
