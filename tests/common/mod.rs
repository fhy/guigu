//! 测试共享 helper：fake provider、测试工具、脚本与配置构造。
//!
//! 供 `runtime_loop.rs` 与 `runtime_loop_tools.rs` 通过 `#[path]` 引入
//! （tests/ 下顶层 .rs 会被当作独立 test crate，故共享代码放子模块）。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use guigu::core::message::{
    AssistantContent, AssistantMessage, Message, StopReason, ThinkingLevel, ToolCall,
    UserContent, UserMessage,
};
use guigu::core::provider::{
    AssistantEvent, AssistantStream, ModelProvider, ProviderError, ProviderRequest,
};
use guigu::core::tool::{ResourceScope, Tool, ToolError, ToolResult};
use guigu::core::{AgentConfig, AgentRuntime, LoopConfig, Model, ToolExecutionMode};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// 脚本化 provider：按 turn 顺序回放 `AssistantEvent`；可模拟建立失败与 gate。
pub struct FakeProvider {
    pub turns: Vec<Vec<AssistantEvent>>,
    pub call_index: AtomicUsize,
    pub call_count: AtomicUsize,
    pub fail_next: AtomicUsize,
    pub last_context_size: AtomicUsize,
    /// 首次 stream() 前等待的信号（用于确定性地在 run 进行中注入命令）。
    pub gate: Mutex<Option<oneshot::Receiver<()>>>,
}

impl FakeProvider {
    pub fn new(turns: Vec<Vec<AssistantEvent>>) -> Arc<Self> {
        Arc::new(Self::with(turns, 0, None))
    }

    /// `fail_next`：前 N 次 stream() 建立失败；`gate`：首次 stream() 前等待。
    pub fn with(
        turns: Vec<Vec<AssistantEvent>>,
        fail_next: usize,
        gate: Option<oneshot::Receiver<()>>,
    ) -> Arc<Self> {
        Arc::new(FakeProvider {
            turns,
            call_index: AtomicUsize::new(0),
            call_count: AtomicUsize::new(0),
            fail_next: AtomicUsize::new(fail_next),
            last_context_size: AtomicUsize::new(0),
            gate: Mutex::new(gate),
        })
    }

    pub fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }

    pub fn last_context_size(&self) -> usize {
        self.last_context_size.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ModelProvider for FakeProvider {
    async fn stream(
        &self,
        request: ProviderRequest,
    ) -> Result<AssistantStream, ProviderError> {
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
        self.last_context_size
            .store(request.context.messages.len(), Ordering::SeqCst);
        let idx = self.call_index.fetch_add(1, Ordering::SeqCst);
        let events = self.turns.get(idx).cloned().unwrap_or_default();
        Ok(Box::pin(stream::iter(events)))
    }
}

/// 顺序记录工具：execute 时取一个递增序号写进结果（验证执行顺序）。
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

/// 并发跟踪工具：记录同时在飞的最大并发数（验证并行/独占）。
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

/// 纯文本 turn 脚本：[TextDelta, Done]。
pub fn text_turn(text: &str) -> Vec<AssistantEvent> {
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

/// 工具调用 turn 脚本：[ToolCallStart, ToolCallEnd, Done]。
pub fn tool_call_turn(id: &str, name: &str, args: &str) -> Vec<AssistantEvent> {
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

pub fn make_config() -> AgentConfig {
    AgentConfig