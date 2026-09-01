//! 工具惰性加载（Deferred Tools）：schema 元数据与执行体分离。
//!
//! `DeferredToolSpec` 是工具 schema（name/description/parameters/resource_scope）的
//! owned 表示，常驻内存供组装 system prompt / 注册表使用。`DeferredTool` 是
//! 实现 `Tool` trait 的惰性包装器：schema 方法只读 `spec`、绝不触发工厂；
//! 执行体在首次 `execute` 时经工厂构建并缓存（进程内仅一次，`OnceLock` 保证
//! 并发下也只构建一次，且 `get_or_init` 闭包同步执行完毕即释放，不跨 await 持锁）。
//!
//! 零破坏：`DeferredTool` 本身是合法 `Tool`，仍放入 `Vec<Arc<dyn Tool>>`，
//! 不改 003 主循环与 `AgentRuntime.tools` 注册契约。

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::core::tool::{ResourceScope, Tool, ToolError, ToolResult};

/// 工具 schema 元数据的 owned 表示，与执行体分离，供 defer 场景常驻。
#[derive(Debug, Clone)]
pub struct DeferredToolSpec {
    /// 工具名（唯一标识，供 LLM toolCall 引用）。
    pub name: String,
    /// 工具描述（供 LLM 理解用途）。
    pub description: String,
    /// 参数 schema（`None` 表示不约束；二期接 schemars）。
    pub parameters: Option<serde_json::Value>,
    /// 资源声明：决定并发安全性。
    pub resource_scope: ResourceScope,
}

impl DeferredToolSpec {
    /// 从已实例化工具抽取 schema（`&str` → `String`，`parameters` 直接 move）。
    pub fn from_tool(tool: &dyn Tool) -> Self {
        Self {
            name: tool.name().to_string(),
            description: tool.description().to_string(),
            parameters: tool.parameters(),
            resource_scope: tool.resource_scope(),
        }
    }
}

/// 惰性工具：schema 常驻，执行体首次 `execute` 时经工厂构建并缓存（进程内仅一次）。
pub struct DeferredTool {
    spec: DeferredToolSpec,
    factory: Box<dyn Fn() -> Arc<dyn Tool> + Send + Sync>,
    inner: OnceLock<Arc<dyn Tool>>,
}

impl DeferredTool {
    /// spec + 工厂闭包。工厂须 infallible（返回 `Arc<dyn Tool>`，不返回 `Result`）。
    pub fn new(
        spec: DeferredToolSpec,
        factory: Box<dyn Fn() -> Arc<dyn Tool> + Send + Sync>,
    ) -> Self {
        Self {
            spec,
            factory,
            inner: OnceLock::new(),
        }
    }

    /// 便捷构造：工厂闭包自动 Box。
    pub fn lazy(
        spec: DeferredToolSpec,
        factory: impl Fn() -> Arc<dyn Tool> + Send + Sync + 'static,
    ) -> Self {
        Self::new(spec, Box::new(factory))
    }

    /// 已实例化工具直接包装：schema 从工具抽取，工厂返回既有 `Arc` 的 clone，
    /// 不产生额外构建。
    pub fn ready(tool: Arc<dyn Tool>) -> Self {
        let spec = DeferredToolSpec::from_tool(&*tool);
        let captured = Arc::clone(&tool);
        Self {
            spec,
            factory: Box::new(move || Arc::clone(&captured)),
            inner: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Tool for DeferredTool {
    fn name(&self) -> &str {
        &self.spec.name
    }

    fn description(&self) -> &str {
        &self.spec.description
    }

    fn parameters(&self) -> Option<serde_json::Value> {
        self.spec.parameters.clone()
    }

    fn resource_scope(&self) -> ResourceScope {
        self.spec.resource_scope
    }

    async fn execute(
        &self,
        tool_call_id: &str,
        args: serde_json::Value,
        signal: CancellationToken,
        on_update: Option<&(dyn Fn(ToolResult) + Send + Sync)>,
    ) -> Result<ToolResult, ToolError> {
        // 首次 execute 才构建执行体；OnceLock 保证并发下也只构建一次，
        // 且 get_or_init 闭包同步执行完毕即释放，不跨 await 持锁。
        let tool = self.inner.get_or_init(|| (self.factory)());
        tool.execute(tool_call_id, args, signal, on_update).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::core::message::ToolResultContent;
    use crate::tools::EchoTool;

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

    fn spec(name: &str, scope: ResourceScope) -> DeferredToolSpec {
        DeferredToolSpec {
            name: name.to_string(),
            description: format!("desc of {name}"),
            parameters: Some(serde_json::json!({ "type": "object" })),
            resource_scope: scope,
        }
    }

    /// 惰性性：构造后工厂计数 0；调用 schema 方法后仍 0。
    #[test]
    fn test_laziness_schema_methods_do_not_build() {
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        let tool = DeferredTool::lazy(spec("lazy", ResourceScope::ReadOnly), move || {
            c.fetch_add(1, Ordering::SeqCst);
            Arc::new(EchoTool)
        });
        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "factory must not run on construct"
        );
        let _ = tool.name();
        let _ = tool.description();
        let _ = tool.parameters();
        let _ = tool.resource_scope();
        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "schema methods must not build"
        );
    }

    /// 首次 execute 构建一次，再次 execute 复用缓存（计数仍 1）。
    #[tokio::test]
    async fn test_execute_builds_once_and_caches() {
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        let tool = DeferredTool::lazy(spec("echo", ResourceScope::ReadOnly), move || {
            c.fetch_add(1, Ordering::SeqCst);
            Arc::new(EchoTool)
        });
        let r1 = tool
            .execute(
                "c1",
                serde_json::json!({ "message": "hi" }),
                CancellationToken::new(),
                None,
            )
            .await
            .expect("first execute should succeed");
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "factory runs once on first execute"
        );
        assert!(!r1.is_error);

        let r2 = tool
            .execute(
                "c2",
                serde_json::json!({ "message": "yo" }),
                CancellationToken::new(),
                None,
            )
            .await
            .expect("second execute should succeed");
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "cached, must not rebuild"
        );
        assert!(!r2.is_error);
    }

    /// schema 转发：name/description/parameters/resource_scope 与 DeferredToolSpec 完全一致。
    #[test]
    fn test_schema_forwarding() {
        let params = Some(serde_json::json!({ "type": "object", "x": 1 }));
        let tool = DeferredTool::lazy(
            DeferredToolSpec {
                name: "mytool".into(),
                description: "the desc".into(),
                parameters: params.clone(),
                resource_scope: ResourceScope::Exclusive,
            },
            || Arc::new(EchoTool),
        );
        assert_eq!(tool.name(), "mytool");
        assert_eq!(tool.description(), "the desc");
        assert_eq!(tool.parameters(), params);
        assert_eq!(tool.resource_scope(), ResourceScope::Exclusive);
    }

    /// execute 透传：tool_call_id/args 原样转发，结果与内层工具一致。
    #[tokio::test]
    async fn test_execute_passthrough() {
        let received = Arc::new(Mutex::new((String::new(), serde_json::Value::Null)));
        let rec = received.clone();
        let tool = DeferredTool::lazy(spec("rec", ResourceScope::ReadOnly), move || {
            Arc::new(RecordingTool {
                received: rec.clone(),
            })
        });
        let args = serde_json::json!({ "k": "v", "n": 3 });
        let result = tool
            .execute("call-42", args.clone(), CancellationToken::new(), None)
            .await
            .expect("execute should succeed");
        let (id, got_args) = received.lock().unwrap().clone();
        assert_eq!(id, "call-42");
        assert_eq!(got_args, args);
        assert_eq!(
            result.content[0],
            ToolResultContent::Text {
                text: "rec:call-42".to_string()
            }
        );
        assert!(!result.is_error);
    }

    /// ready：包装已实例化工具，name 正确且可直接 execute。
    #[tokio::test]
    async fn test_ready_wraps_existing_tool() {
        let tool = DeferredTool::ready(Arc::new(EchoTool));
        assert_eq!(tool.name(), "echo");
        assert_eq!(tool.resource_scope(), ResourceScope::ReadOnly);
        let result = tool
            .execute(
                "c1",
                serde_json::json!({ "message": "ping" }),
                CancellationToken::new(),
                None,
            )
            .await
            .expect("ready tool should execute");
        assert!(!result.is_error);
        assert_eq!(
            result.content[0],
            ToolResultContent::Text {
                text: "ping".to_string()
            }
        );
    }

    /// from_tool：从 &dyn Tool 正确抽取四项 schema。
    #[test]
    fn test_from_tool_extracts_schema() {
        let base: Arc<dyn Tool> = Arc::new(EchoTool);
        let spec = DeferredToolSpec::from_tool(&*base);
        assert_eq!(spec.name, "echo");
        assert_eq!(spec.description, "Echo back the provided message.");
        assert!(spec.parameters.is_some());
        assert_eq!(spec.resource_scope, ResourceScope::ReadOnly);
    }
}
