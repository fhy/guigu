
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
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
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

// ---------- Fake provider ----------

/// 脚本化 provider：按 turn 顺序回放 `AssistantEvent`；可模拟建立失败与 gate。
struct FakeProvider {
    turns: Vec<Vec<AssistantEvent>>,
    call_index: AtomicUsize,
    call_count: AtomicUsize,
    fail_next: AtomicUsize,
    scripted_errors: Mutex<VecDeque<ProviderError>>,
    last_context_size: AtomicUsize,
    /// 首次 stream() 前等待的信号（用于确定性地在 run 进行中注入命令）。
    gate: Mutex<Option<oneshot::Receiver<()>>>,
}

impl FakeProvider {
    fn new(turns: Vec<Vec<AssistantEvent>>) -> Arc<Self> {
        Self::with(turns, 0, None)
    }

    /// `fail_next`：前 N 次 stream() 建立失败；`gate`：首次 stream() 前等待。
    fn with(
        turns: Vec<Vec<AssistantEvent>>,
        fail_next: usize,
        gate: Option<oneshot::Receiver<()>>,
    ) -> Arc<Self> {
        Arc::new(FakeProvider {
            turns,
            call_index: AtomicUsize::new(0),
            call_count: AtomicUsize::new(0),
            fail_next: AtomicUsize::new(fail_next),
            scripted_errors: Mutex::new(VecDeque::new()),
            last_context_size: AtomicUsize::new(0),
            gate: Mutex::new(gate),
        })
    }

    fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }

    fn last_context_size(&self) -> usize {
        self.last_context_size.load(Ordering::SeqCst)
    }

    fn with_errors(turns: Vec<Vec<AssistantEvent>>, errors: Vec<ProviderError>) -> Arc<Self> {
        let provider = Self::new(turns);
        *provider.scripted_errors.lock().expect("error mutex") = errors.into();
        provider
    }
}

#[async_trait]
impl ModelProvider for FakeProvider {
    async fn stream(&self, request: ProviderRequest) -> Result<AssistantStream, ProviderError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        // gate：首次调用前等待（确定性注入命令）。
        let rx = self.gate.lock().expect("gate mutex").take();
        if let Some(rx) = rx {
            let _ = rx.await;
        }
        // 模拟建立失败。
        let remaining = self.fail_next.load(Ordering::SeqCst);
        if remaining > 0 {
            self.fail_next.fetch_sub(1, Ordering::SeqCst);
            return Err(ProviderError::Request(
                "simulated establishment failure".to_string(),
            ));
        }
        if let Some(error) = self
            .scripted_errors
            .lock()
            .expect("error mutex")
            .pop_front()
        {
            return Err(error);
        }
        self.last_context_size
            .store(request.context.messages.len(), Ordering::SeqCst);
        let idx = self.call_index.fetch_add(1, Ordering::SeqCst);
        let events = self.turns.get(idx).cloned().unwrap_or_default();
        Ok(Box::pin(stream::iter(events)))
    }
}

/// 挂起 provider：`stream()` 永不返回（`pending()` future），用于验证建流阶段
/// 的取消/超时（Task 040）。runtime 的 `select!` 应在 provider 返回前抢先取消。
struct HangingProvider {
    call_count: AtomicUsize,
}

impl HangingProvider {
    fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ModelProvider for HangingProvider {
    async fn stream(&self, _request: ProviderRequest) -> Result<AssistantStream, ProviderError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        // 挂起：永不返回（runtime 的 select! 应抢先取消/超时）。
        futures::future::pending::<()>().await;
        Err(ProviderError::Request("unreachable".to_string()))
    }
}

// ---------- 测试工具 ----------

/// 顺序记录工具：execute 时取一个递增序号写进结果（验证执行顺序）。
struct SeqTool {
    name: String,
    counter: Arc<AtomicUsize>,
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

/// 并发跟踪工具：记录同时在飞的最大并发数（验证并行/独占）。
struct ConcurrencyTool {
    name: String,
    scope: ResourceScope,
    in_flight: Arc<AtomicUsize>,
    max_in_flight: Arc<AtomicUsize>,
    delay_ms: u64,
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

// ---------- 脚本与配置 ----------

