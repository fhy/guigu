use async_trait::async_trait;
use guigu::core::tool::{ResourceScope, Tool, ToolError, ToolResult};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub struct SeqTool {
    pub name: String,
    pub counter: Arc<AtomicUsize>,
}
#[async_trait]
impl Tool for SeqTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "records execution order"
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
        let seq = self.counter.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult::text(format!("{}:{}", self.name, seq)))
    }
}

pub struct ConcurrencyTool {
    pub name: String,
    pub scope: ResourceScope,
    pub in_flight: Arc<AtomicUsize>,
    pub max_in_flight: Arc<AtomicUsize>,
    pub delay_ms: u64,
}
#[async_trait]
impl Tool for ConcurrencyTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "tracks concurrency"
    }
    fn resource_scope(&self) -> ResourceScope {
        self.scope
    }
    async fn execute(
        &self,
        _id: &str,
        _args: serde_json::Value,
        _signal: CancellationToken,
        _on_update: Option<&(dyn Fn(ToolResult) + Send + Sync)>,
    ) -> Result<ToolResult, ToolError> {
        let cur = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(cur, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        Ok(ToolResult::text(self.name.clone()))
    }
}
