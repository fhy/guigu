//! [`PluginTool`]：实现 [`Tool`] 的异步惰性包装器。
//!
//! schema（name/description/parameters/resource_scope）常驻自 [`DeferredToolSpec`]，
//! **绝不触发**实例化；执行体在首次 `execute` 时经 [`Plugin::instantiate`] 异步构建
//! 并缓存（`tokio::sync::OnceCell::get_or_try_init` 保证并发下也只构建一次，内部是
//! async 协调，不跨 await 持 std 锁）。实例化**失败不缓存、可重试**——与 011
//! `DeferredTool`（infallible，失败即 panic）的关键差异：插件实例化可能因 IO/远程
//! 暂时失败，允许下一次 `execute` 重试。
//!
//! 零破坏：`PluginTool` 本身是合法 [`Tool`]，可入 `Vec<Arc<dyn Tool>>`。

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::core::tool::{ResourceScope, Tool, ToolError, ToolResult};
use crate::plugin::{Plugin, PluginError};
use crate::tools::DeferredToolSpec;

/// 异步惰性工具：schema 常驻，执行体首次 `execute` 时异步构建并缓存；失败不缓存、可重试。
pub struct PluginTool {
    spec: DeferredToolSpec,
    plugin: Arc<dyn Plugin>,
    name: String,
    inner: tokio::sync::OnceCell<Arc<dyn Tool>>,
}

impl PluginTool {
    /// `spec.name` 与 `name` 一致；`plugin` 与 `spec` 由 [`PluginRegistry::tools`] 装配时注入。
    pub fn new(plugin: Arc<dyn Plugin>, spec: DeferredToolSpec) -> Self {
        let name = spec.name.clone();
        Self {
            spec,
            plugin,
            name,
            inner: tokio::sync::OnceCell::new(),
        }
    }
}

/// 将 [`PluginError`] 映射为 [`ToolError`]（执行体实例化失败）。
///
/// 复用 003 `ToolError`（struct 带 `message` 字段，可承载字符串），取 `PluginError`
/// 的 `Display`（`Instantiation` 变体经 `#[source]` 链式展开底层错误）。
fn plugin_to_tool_error(err: PluginError) -> ToolError {
    ToolError::new(err.to_string())
}

#[async_trait]
impl Tool for PluginTool {
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
        // 首次 execute 才异步构建执行体；OnceCell::get_or_try_init 保证并发下也只
        // 构建一次，失败不落 cell（下次重试），不跨 await 持 std 锁。
        let tool = self
            .inner
            .get_or_try_init(|| self.plugin.instantiate(&self.name))
            .await
            .map_err(plugin_to_tool_error)?;
        tool.execute(tool_call_id, args, signal, on_update).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::tools::EchoTool;

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

    /// 抖动型 fake plugin：前 `fail_until` 次 `instantiate` 失败，之后成功。
    struct FlakyPlugin {
        id: String,
        specs: Vec<DeferredToolSpec>,
        calls: Arc<AtomicUsize>,
        fail_until: usize,
    }

    #[async_trait]
    impl Plugin for FlakyPlugin {
        fn id(&self) -> &str {
            &self.id
        }
        fn tools(&self) -> Vec<DeferredToolSpec> {
            self.specs.clone()
        }
        async fn instantiate(&self, name: &str) -> Result<Arc<dyn Tool>, PluginError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if n <= self.fail_until {
                return Err(PluginError::Instantiation {
                    plugin: self.id.clone(),
                    tool: name.to_string(),
                    source: Box::new(std::io::Error::other("simulated transient failure")),
                });
            }
            if self.specs.iter().any(|s| s.name == name) {
                Ok(Arc::new(EchoTool))
            } else {
                Err(PluginError::ToolNotDeclared(name.to_string()))
            }
        }
    }

    /// 惰性性：构造后 instantiate 计数 0；调用 schema 四方法后仍 0。
    #[test]
    fn test_laziness_schema_methods_do_not_instantiate() {
        let calls = Arc::new(AtomicUsize::new(0));
        let plugin = Arc::new(CountingPlugin {
            id: "p".into(),
            specs: vec![spec("echo", ResourceScope::ReadOnly)],
            calls: calls.clone(),
        });
        let tool = PluginTool::new(plugin, spec("echo", ResourceScope::ReadOnly));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "must not instantiate on construct"
        );
        let _ = tool.name();
        let _ = tool.description();
        let _ = tool.parameters();
        let _ = tool.resource_scope();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "schema methods must not instantiate"
        );
    }

    /// 首次 execute 实例化一次，再次 execute 复用缓存（计数仍 1）。
    #[tokio::test]
    async fn test_execute_instantiates_once_and_caches() {
        let calls = Arc::new(AtomicUsize::new(0));
        let plugin = Arc::new(CountingPlugin {
            id: "p".into(),
            specs: vec![spec("echo", ResourceScope::ReadOnly)],
            calls: calls.clone(),
        });
        let tool = PluginTool::new(plugin, spec("echo", ResourceScope::ReadOnly));
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
            calls.load(Ordering::SeqCst),
            1,
            "instantiate once on first execute"
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
            calls.load(Ordering::SeqCst),
            1,
            "cached, must not re-instantiate"
        );
        assert!(!r2.is_error);
    }

    /// 失败可重试：首次 instantiate 失败 → execute 返回 ToolError；第二次成功，计数 2。
    #[tokio::test]
    async fn test_failure_not_cached_retryable() {
        let calls = Arc::new(AtomicUsize::new(0));
        let plugin = Arc::new(FlakyPlugin {
            id: "p".into(),
            specs: vec![spec("echo", ResourceScope::ReadOnly)],
            calls: calls.clone(),
            fail_until: 1,
        });
        let tool = PluginTool::new(plugin, spec("echo", ResourceScope::ReadOnly));
        // 首次 execute 失败（instantiate 第 1 次失败）
        let r1 = tool
            .execute(
                "c1",
                serde_json::json!({ "message": "hi" }),
                CancellationToken::new(),
                None,
            )
            .await;
        assert!(r1.is_err(), "first execute should fail");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // 第二次 execute 成功（instantiate 第 2 次成功），证明失败不缓存
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
            calls.load(Ordering::SeqCst),
            2,
            "failure not cached, retried on next execute"
        );
        assert!(!r2.is_error);
    }

    /// schema 转发：四方法与 DeferredToolSpec 完全一致。
    #[test]
    fn test_schema_forwarding() {
        let params = Some(serde_json::json!({ "type": "object", "x": 1 }));
        let spec = DeferredToolSpec {
            name: "mytool".into(),
            description: "the desc".into(),
            parameters: params.clone(),
            resource_scope: ResourceScope::Exclusive,
        };
        let plugin = Arc::new(CountingPlugin {
            id: "p".into(),
            specs: vec![spec.clone()],
            calls: Arc::new(AtomicUsize::new(0)),
        });
        let tool = PluginTool::new(plugin, spec);
        assert_eq!(tool.name(), "mytool");
        assert_eq!(tool.description(), "the desc");
        assert_eq!(tool.parameters(), params);
        assert_eq!(tool.resource_scope(), ResourceScope::Exclusive);
    }
}
