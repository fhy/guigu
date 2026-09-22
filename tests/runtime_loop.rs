//! Task 003 主循环行为测试：用 fake provider 驱动，覆盖一期行为契约。
//!
//! 覆盖验收分支：
//! - 纯文本一轮结束
//! - toolCall→ToolResult 循环
//! - 顺序执行顺序保证
//! - ReadOnly 并行
//! - Exclusive 独占
//! - steering / followUp
//! - abort 后产出 stop_reason: Aborted
//! - provider 失败重试（计数可断言）
//! - 上下文预算超限触发截断
//!
//! 同步点：一律以 `wait_for_idle` 为同步点；steering/followUp 用 provider
//! gate（oneshot）确定性地在 run 进行中注入命令，不用 sleep 竞态。

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

/// 指定 `stop_reason` 的 tool call turn（Task 040 Length 保护测试用）。
fn tool_call_turn_with_stop(
    id: &str,
    name: &str,
    args: &str,
    stop: StopReason,
) -> Vec<AssistantEvent> {
    let message = AssistantMessage {
        content: vec![AssistantContent::ToolCall(ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: args.to_string(),
        })],
        model: None,
        usage: None,
        stop_reason: Some(stop),
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

/// 多工具调用 turn：所有 toolCall 的 Start/End 事件 + 末尾**单个** `Done`
/// （message 含全部 toolCall）。真实 provider 一个 turn 只发一个 `Done`。
fn multi_tool_call_turn(calls: &[(&str, &str, &str)]) -> Vec<AssistantEvent> {
    let mut events = Vec::new();
    let mut content = Vec::new();
    for (id, name, args) in calls {
        events.push(AssistantEvent::ToolCallStart {
            id: id.to_string(),
            name: name.to_string(),
            arguments: args.to_string(),
        });
        events.push(AssistantEvent::ToolCallEnd { id: id.to_string() });
        content.push(AssistantContent::ToolCall(ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: args.to_string(),
        }));
    }
    let message = AssistantMessage {
        content,
        model: None,
        usage: None,
        stop_reason: Some(StopReason::Completed),
        error_message: None,
        timestamp: 0,
    };
    events.push(AssistantEvent::Done { message });
    events
}

/// 多工具调用 turn（指定 `stop_reason`）：每个 toolCall 的 Start/End 事件 +
/// 末尾**单个** `Done`（message 含全部 toolCall）。`delta_ids` 中的 toolCall 经
/// `ToolCallStart`（空参数）+ `ToolCallDelta`（累积完整参数）+ `ToolCallEnd` 形成，
/// 其余用 `ToolCallStart`（完整参数）+ `ToolCallEnd`。用于 Task 040 Length 保护
/// 测试覆盖 delta 累积路径与逐调用生命周期事件。
fn multi_tool_call_turn_with_stop(
    calls: &[(&str, &str, &str)],
    delta_ids: &[&str],
    stop: StopReason,
) -> Vec<AssistantEvent> {
    let mut events = Vec::new();
    let mut content = Vec::new();
    for (id, name, args) in calls {
        if delta_ids.contains(id) {
            // delta 路径：Start（空参数）→ Delta（累积完整参数）→ End。
            events.push(AssistantEvent::ToolCallStart {
                id: id.to_string(),
                name: name.to_string(),
                arguments: String::new(),
            });
            events.push(AssistantEvent::ToolCallDelta {
                id: id.to_string(),
                arguments_delta: args.to_string(),
            });
            events.push(AssistantEvent::ToolCallEnd { id: id.to_string() });
        } else {
            events.push(AssistantEvent::ToolCallStart {
                id: id.to_string(),
                name: name.to_string(),
                arguments: args.to_string(),
            });
            events.push(AssistantEvent::ToolCallEnd { id: id.to_string() });
        }
        content.push(AssistantContent::ToolCall(ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: args.to_string(),
        }));
    }
    let message = AssistantMessage {
        content,
        model: None,
        usage: None,
        stop_reason: Some(stop),
        error_message: None,
        timestamp: 0,
    };
    events.push(AssistantEvent::Done { message });
    events
}

fn make_config() -> AgentConfig {
    AgentConfig {
        system_prompt: "test".to_string(),
        model: Some("test-model".to_string()),
        thinking_level: ThinkingLevel::Off,
    }
}

fn make_runtime(
    provider: Arc<dyn ModelProvider>,
    tools: Vec<Arc<dyn Tool>>,
    mode: ToolExecutionMode,
    context_window: u32,
) -> AgentRuntime {
    AgentRuntime {
        provider,
        tools,
        loop_config: LoopConfig {
            model: Model {
                id: "test-model".to_string(),
                context_window,
            },
            tool_execution: mode,
            retry_base_delay: Duration::from_millis(1),
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

// ---------- 测试 ----------

/// 纯文本一轮结束：无 toolCall → 单 turn 后退出。
#[tokio::test]
async fn test_pure_text_single_turn() {
    let provider = FakeProvider::new(vec![text_turn("hello")]);
    let handle = AgentHandle::spawn(
        make_config(),
        make_runtime(
            provider.clone(),
            Vec::new(),
            ToolExecutionMode::Sequential,
            8192,
        ),
    );
    handle
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");
    handle.wait_for_idle().await.expect("should settle");

    assert_eq!(provider.call_count(), 1, "provider called once");
    let snapshot = handle.snapshot();
    assert_eq!(snapshot.messages.len(), 2, "user + assistant");
    assert!(
        matches!(snapshot.messages[1].as_ref(), Message::Assistant(_)),
        "second message should be assistant"
    );
}

/// toolCall→ToolResult 循环：turn1 工具调用 → 执行 → turn2 文本 → 退出。
#[tokio::test]
async fn test_tool_call_loop() {
    let provider = FakeProvider::new(vec![tool_call_turn("c1", "seq", "{}"), text_turn("done")]);
    let counter = Arc::new(AtomicUsize::new(0));
    let tools = vec![Arc::new(SeqTool {
        name: "seq".to_string(),
        counter: counter.clone(),
    }) as Arc<dyn Tool>];
    let handle = AgentHandle::spawn(
        make_config(),
        make_runtime(provider.clone(), tools, ToolExecutionMode::Sequential, 8192),
    );
    handle
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");
    handle.wait_for_idle().await.expect("should settle");

    assert_eq!(provider.call_count(), 2, "two turns");
    assert_eq!(counter.load(Ordering::SeqCst), 1, "tool executed once");
    let snapshot = handle.snapshot();
    // user + assistant(toolcall) + toolresult + assistant(text)
    assert_eq!(snapshot.messages.len(), 4, "expected 4 messages");
    assert!(
        matches!(snapshot.messages[2].as_ref(), Message::ToolResult(_)),
        "third message should be ToolResult"
    );
}

/// 顺序执行顺序保证：Sequential 下工具按 toolCall 顺序执行。
#[tokio::test]
async fn test_sequential_order() {
    let provider = FakeProvider::new(vec![
        // 三个工具调用（同一 turn，末尾单个 Done）
        multi_tool_call_turn(&[("c1", "t1", "{}"), ("c2", "t2", "{}"), ("c3", "t3", "{}")]),
        text_turn("done"),
    ]);
    let counter = Arc::new(AtomicUsize::new(0));
    let tools: Vec<Arc<dyn Tool>> = (1..4)
        .map(|i| {
            Arc::new(SeqTool {
                name: format!("t{i}"),
                counter: counter.clone(),
            }) as Arc<dyn Tool>
        })
        .collect();
    let handle = AgentHandle::spawn(
        make_config(),
        make_runtime(provider.clone(), tools, ToolExecutionMode::Sequential, 8192),
    );
    handle
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");
    handle.wait_for_idle().await.expect("should settle");

    let snapshot = handle.snapshot();
    let texts = tool_result_texts(&snapshot.messages);
    assert_eq!(texts.len(), 3, "three tool results");
    // 顺序：t1:0, t2:1, t3:2
    assert_eq!(texts[0], "t1:0", "first tool is t1 with seq 0");
    assert_eq!(texts[1], "t2:1", "second tool is t2 with seq 1");
    assert_eq!(texts[2], "t3:2", "third tool is t3 with seq 2");
}

/// ReadOnly 并行：ReadOnlyParallel 下连续 ReadOnly 工具并发执行。
#[tokio::test]
async fn test_readonly_parallel() {
    let provider = FakeProvider::new(vec![
        multi_tool_call_turn(&[("c1", "p1", "{}"), ("c2", "p2", "{}"), ("c3", "p3", "{}")]),
        text_turn("done"),
    ]);
    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_in_flight = Arc::new(AtomicUsize::new(0));
    let tools: Vec<Arc<dyn Tool>> = (0..3)
        .map(|i| {
            Arc::new(ConcurrencyTool {
                name: format!("p{i}"),
                scope: ResourceScope::ReadOnly,
                in_flight: in_flight.clone(),
                max_in_flight: max_in_flight.clone(),
                delay_ms: 50,
            }) as Arc<dyn Tool>
        })
        .collect();
    let handle = AgentHandle::spawn(
        make_config(),
        make_runtime(
            provider.clone(),
            tools,
            ToolExecutionMode::ReadOnlyParallel,
            8192,
        ),
    );
    handle
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");
    handle.wait_for_idle().await.expect("should settle");

    assert!(
        max_in_flight.load(Ordering::SeqCst) >= 2,
        "ReadOnly tools should run in parallel (max in-flight >= 2), got {}",
        max_in_flight.load(Ordering::SeqCst)
    );
}

/// Exclusive 独占：Exclusive 工具打断 ReadOnly 并行组（不与其他工具并行）。
#[tokio::test]
async fn test_exclusive() {
    let provider = FakeProvider::new(vec![
        multi_tool_call_turn(&[("c1", "r1", "{}"), ("c2", "ex", "{}"), ("c3", "r2", "{}")]),
        text_turn("done"),
    ]);
    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_in_flight = Arc::new(AtomicUsize::new(0));
    let mk = |name: &str, scope: ResourceScope| {
        Arc::new(ConcurrencyTool {
            name: name.to_string(),
            scope,
            in_flight: in_flight.clone(),
            max_in_flight: max_in_flight.clone(),
            delay_ms: 50,
        }) as Arc<dyn Tool>
    };
    let tools = vec![
        mk("r1", ResourceScope::ReadOnly),
        mk("ex", ResourceScope::Exclusive),
        mk("r2", ResourceScope::ReadOnly),
    ];
    let handle = AgentHandle::spawn(
        make_config(),
        make_runtime(
            provider.clone(),
            tools,
            ToolExecutionMode::ReadOnlyParallel,
            8192,
        ),
    );
    handle
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");
    handle.wait_for_idle().await.expect("should settle");

    // Exclusive 打断并行组：每个组只有 1 个工具 → 最大并发 1。
    assert_eq!(
        max_in_flight.load(Ordering::SeqCst),
        1,
        "Exclusive should break the parallel group (max in-flight == 1)"
    );
}

/// steering：run 进行中注入 Steer → 在 no-tool 边界注入并继续。
#[tokio::test]
async fn test_steering() {
    let (gate_tx, gate_rx) = oneshot::channel();
    let provider = FakeProvider::with(vec![text_turn("a"), text_turn("b")], 0, Some(gate_rx));
    let handle = AgentHandle::spawn(
        make_config(),
        make_runtime(
            provider.clone(),
            Vec::new(),
            ToolExecutionMode::Sequential,
            8192,
        ),
    );
    handle
        .prompt(vec![user_msg("initial")])
        .await
        .expect("prompt should succeed");
    // run 进行中（provider 在 gate 等待）注入 Steer，然后放行。
    handle
        .steer(user_msg("steered"))
        .await
        .expect("steer should succeed");
    gate_tx.send(()).expect("gate should receive");
    handle.wait_for_idle().await.expect("should settle");

    let snapshot = handle.snapshot();
    // user(initial) + assistant(a) + user(steered) + assistant(b)
    assert_eq!(
        snapshot.messages.len(),
        4,
        "steer should inject and continue"
    );
    assert!(
        matches!(snapshot.messages[2].as_ref(), Message::User(_)),
        "third message should be the steered user message"
    );
}

/// followUp：run 即将退出时注入 FollowUp → 继续一轮。
#[tokio::test]
async fn test_followup() {
    let (gate_tx, gate_rx) = oneshot::channel();
    let provider = FakeProvider::with(vec![text_turn("a"), text_turn("b")], 0, Some(gate_rx));
    let handle = AgentHandle::spawn(
        make_config(),
        make_runtime(
            provider.clone(),
            Vec::new(),
            ToolExecutionMode::Sequential,
            8192,
        ),
    );
    handle
        .prompt(vec![user_msg("initial")])
        .await
        .expect("prompt should succeed");
    handle
        .follow_up(user_msg("followed"))
        .await
        .expect("follow_up should succeed");
    gate_tx.send(()).expect("gate should receive");
    handle.wait_for_idle().await.expect("should settle");

    let snapshot = handle.snapshot();
    assert_eq!(
        snapshot.messages.len(),
        4,
        "followUp should inject and continue"
    );
}

/// abort：run 进行中 abort → 产出 stop_reason: Aborted，AgentEnd 必达。
#[tokio::test]
async fn test_abort() {
    let (gate_tx, gate_rx) = oneshot::channel();
    let provider = FakeProvider::with(vec![text_turn("a")], 0, Some(gate_rx));
    let handle = AgentHandle::spawn(
        make_config(),
        make_runtime(
            provider.clone(),
            Vec::new(),
            ToolExecutionMode::Sequential,
            8192,
        ),
    );
    let mut rx = handle.subscribe();
    handle
        .prompt(vec![user_msg("initial")])
        .await
        .expect("prompt should succeed");
    // 等 run 进入 streaming（AgentStart）后 abort。
    wait_event(&mut rx, |e| {
        matches!(e, guigu::core::event::AgentEvent::AgentStart)
    })
    .await
    .expect("should receive AgentStart");
    handle.abort();
    gate_tx.send(()).expect("gate should receive");
    wait_event(&mut rx, |e| {
        matches!(e, guigu::core::event::AgentEvent::AgentEnd { .. })
    })
    .await
    .expect("AgentEnd should be delivered");
    handle.wait_for_idle().await.expect("should settle");

    let snapshot = handle.snapshot();
    assert!(
        !snapshot.is_streaming,
        "is_streaming should be false after abort"
    );
    // 规格要求：abort 后产出 stop_reason: Aborted。
    let last = snapshot
        .messages
        .last()
        .expect("transcript should not be empty");
    let Message::Assistant(a) = last.as_ref() else {
        panic!("last message should be assistant");
    };
    assert_eq!(
        a.stop_reason,
        Some(StopReason::Aborted),
        "abort should produce stop_reason: Aborted"
    );
}

/// 流结束但未收到 Done（provider 异常截断）→ stop_reason: Error，不掩盖为 Completed。
#[tokio::test]
async fn test_stream_ends_without_done() {
    // 一个 turn 只发 TextDelta、无 Done —— 流直接结束。
    let events = vec![AssistantEvent::TextDelta {
        text: "partial".to_string(),
    }];
    let provider = FakeProvider::new(vec![events]);
    let handle = AgentHandle::spawn(
        make_config(),
        make_runtime(
            provider.clone(),
            Vec::new(),
            ToolExecutionMode::Sequential,
            8192,
        ),
    );
    handle
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");
    handle.wait_for_idle().await.expect("should settle");

    let snapshot = handle.snapshot();
    let last = snapshot
        .messages
        .last()
        .expect("transcript should not be empty");
    let Message::Assistant(a) = last.as_ref() else {
        panic!("last message should be assistant");
    };
    assert_eq!(
        a.stop_reason,
        Some(StopReason::Error),
        "stream ending without Done should produce stop_reason: Error"
    );
    assert!(a.error_message.is_some(), "should carry an error message");
}

/// provider 失败重试：前 2 次建立失败 → 第 3 次成功，call_count == 3。
#[tokio::test]
async fn test_retry() {
    let provider = FakeProvider::with(vec![text_turn("ok")], 2, None);
    let handle = AgentHandle::spawn(
        make_config(),
        make_runtime(
            provider.clone(),
            Vec::new(),
            ToolExecutionMode::Sequential,
            8192,
        ),
    );
    handle
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");
    handle.wait_for_idle().await.expect("should settle");

    assert_eq!(provider.call_count(), 3, "2 failures + 1 success = 3 calls");
    let snapshot = handle.snapshot();
    assert_eq!(
        snapshot.messages.len(),
        2,
        "run should complete after retries"
    );
}

/// 永久 provider 错误不应进入重试循环。
#[tokio::test]
async fn test_permanent_provider_error_is_not_retried() {
    let provider = FakeProvider::with_errors(
        vec![],
        vec![ProviderError::HttpStatus {
            status: 401,
            body: "unauthorized".to_string(),
            retry_after: None,
        }],
    );
    let handle = AgentHandle::spawn(
        make_config(),
        make_runtime(
            provider.clone(),
            Vec::new(),
            ToolExecutionMode::Sequential,
            8192,
        ),
    );
    handle
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");
    handle.wait_for_idle().await.expect("should settle");

    assert_eq!(provider.call_count(), 1, "permanent errors must not retry");
    let last = handle
        .snapshot()
        .messages
        .last()
        .cloned()
        .expect("assistant message");
    let Message::Assistant(message) = last.as_ref() else {
        panic!("expected assistant error message");
    };
    assert_eq!(message.stop_reason, Some(StopReason::Error));
}

/// 429 的 Retry-After 应作为等待时间，并受 retry_max_delay 封顶。
#[tokio::test]
async fn test_rate_limited_retry_after_is_capped() {
    let provider = FakeProvider::with_errors(
        vec![text_turn("ok")],
        vec![ProviderError::HttpStatus {
            status: 429,
            body: "rate limited".to_string(),
            retry_after: Some(Duration::from_millis(80)),
        }],
    );
    let mut runtime = make_runtime(
        provider.clone(),
        Vec::new(),
        ToolExecutionMode::Sequential,
        8192,
    );
    runtime.loop_config.retry_max_delay = Duration::from_millis(10);
    let handle = AgentHandle::spawn(make_config(), runtime);
    let started = tokio::time::Instant::now();
    handle
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");
    handle.wait_for_idle().await.expect("should settle");
    let elapsed = started.elapsed();

    assert_eq!(provider.call_count(), 2, "rate limit should be retried");
    assert!(
        elapsed >= Duration::from_millis(8),
        "retry should wait near cap: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(60),
        "retry-after must be capped: {elapsed:?}"
    );
}

/// Retry-After 小于上限时应直接决定等待时长，而非退回指数延迟。
#[tokio::test]
async fn test_rate_limited_retry_after_is_used() {
    let provider = FakeProvider::with_errors(
        vec![text_turn("ok")],
        vec![ProviderError::HttpStatus {
            status: 429,
            body: "rate limited".to_string(),
            retry_after: Some(Duration::from_millis(20)),
        }],
    );
    let mut runtime = make_runtime(
        provider.clone(),
        Vec::new(),
        ToolExecutionMode::Sequential,
        8192,
    );
    runtime.loop_config.retry_max_delay = Duration::from_secs(1);
    let handle = AgentHandle::spawn(make_config(), runtime);
    let started = tokio::time::Instant::now();
    handle
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");
    handle.wait_for_idle().await.expect("should settle");

    let elapsed = started.elapsed();
    assert_eq!(provider.call_count(), 2);
    assert!(
        elapsed >= Duration::from_millis(15),
        "retry-after was skipped: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(200),
        "unexpected exponential delay: {elapsed:?}"
    );
}

/// 429 缺少 Retry-After 时应回退到指数退避。
#[tokio::test]
async fn test_rate_limited_without_retry_after_uses_exponential_backoff() {
    let provider = FakeProvider::with_errors(
        vec![text_turn("ok")],
        vec![ProviderError::HttpStatus {
            status: 429,
            body: "rate limited".to_string(),
            retry_after: None,
        }],
    );
    let mut runtime = make_runtime(
        provider.clone(),
        Vec::new(),
        ToolExecutionMode::Sequential,
        8192,
    );
    runtime.loop_config.retry_base_delay = Duration::from_millis(10);
    runtime.loop_config.retry_max_delay = Duration::from_millis(50);
    let handle = AgentHandle::spawn(make_config(), runtime);
    let started = tokio::time::Instant::now();
    handle
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");
    handle.wait_for_idle().await.expect("should settle");

    let elapsed = started.elapsed();
    assert_eq!(provider.call_count(), 2);
    assert!(
        elapsed >= Duration::from_millis(8),
        "fallback backoff was skipped: {elapsed:?}"
    );
    assert!(elapsed < Duration::from_millis(100));
}

/// 退避等待期间取消应立即打断 sleep，而不是等待完整退避时长。
#[tokio::test]
async fn test_retry_backoff_can_be_cancelled() {
    let provider = FakeProvider::with_errors(
        vec![],
        vec![ProviderError::Request("temporary".to_string())],
    );
    let handle = AgentHandle::spawn(
        make_config(),
        make_runtime(
            provider.clone(),
            Vec::new(),
            ToolExecutionMode::Sequential,
            8192,
        ),
    );
    handle
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");
    tokio::time::timeout(Duration::from_secs(1), async {
        while provider.call_count() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("provider should be called");
    let started = tokio::time::Instant::now();
    handle
        .clone()
        .shutdown()
        .await
        .expect("shutdown should succeed");
    assert!(started.elapsed() < Duration::from_millis(100));
    assert_eq!(provider.call_count(), 1, "cancelled backoff must not retry");
    let last = handle
        .snapshot()
        .messages
        .last()
        .cloned()
        .expect("assistant message");
    let Message::Assistant(message) = last.as_ref() else {
        panic!("expected assistant abort message");
    };
    assert_eq!(message.stop_reason, Some(StopReason::Aborted));
}

/// 上下文预算超限触发截断：长 transcript + 小窗口 → provider 收到的上下文被截断。
#[tokio::test]
async fn test_context_budget_truncation() {
    // 5 条用户消息，每条 ~101 token；窗口 250 → 截断到 ~2 条。
    let provider = FakeProvider::new(vec![text_turn("ok")]);
    let handle = AgentHandle::spawn(
        make_config(),
        make_runtime(
            provider.clone(),
            Vec::new(),
            ToolExecutionMode::Sequential,
            250,
        ),
    );
    let msgs: Vec<Message> = (0..5)
        .map(|i| user_msg(&format!("m{i}{}", "x".repeat(400))))
        .collect();
    handle.prompt(msgs).await.expect("prompt should succeed");
    handle.wait_for_idle().await.expect("should settle");

    assert!(
        provider.last_context_size() < 5,
        "context should be truncated (got {} messages, expected < 5)",
        provider.last_context_size()
    );
}

// ---------- Task 040：Length 截断保护 + 建流取消/超时 ----------

/// Length 保护：stop_reason == Length 且含 ToolCall → 不执行任何工具，
/// 每个 tool_call 按输入顺序产出 `ToolExecutionStart` → `ToolExecutionEnd{is_error:true}`
/// 生命周期事件，合成错误 ToolResult（is_error: true）入 transcript。
/// 覆盖：≥2 个 ToolCall，其中 c1 经 Start+Delta+End 累积参数（delta 路径），
/// c2 直接给完整参数；断言事件序列、逐调用 is_error、双合成结果入 transcript、
/// 工具执行计数保持 0。
#[tokio::test]
async fn test_length_truncation_protects_tool_calls() {
    let provider = FakeProvider::new(vec![
        multi_tool_call_turn_with_stop(
            &[("c1", "seq", "{\"a\":1}"), ("c2", "seq", "{\"b\":2}")],
            &["c1"],
            StopReason::Length,
        ),
        text_turn("done"),
    ]);
    let counter = Arc::new(AtomicUsize::new(0));
    let tools = vec![Arc::new(SeqTool {
        name: "seq".to_string(),
        counter: counter.clone(),
    }) as Arc<dyn Tool>];
    let handle = AgentHandle::spawn(
        make_config(),
        make_runtime(provider.clone(), tools, ToolExecutionMode::Sequential, 8192),
    );
    let mut rx = handle.subscribe();
    handle
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");
    // 收集事件直到 AgentEnd（含），用于断言逐调用生命周期事件序列。
    let events = collect_until_agent_end(&mut rx).await;
    handle.wait_for_idle().await.expect("should settle");

    // 1. 无任何工具被执行（Length 截断保护）。
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "no tool should be executed on Length truncation"
    );

    // 2. 每个 tool_call 按输入顺序产出 Start → End(is_error=true)。
    let tool_events: Vec<(String, bool)> = events
        .iter()
        .filter_map(|e| match e {
            guigu::core::event::AgentEvent::ToolExecutionStart { tool_call_id, .. } => {
                Some((tool_call_id.clone(), false))
            }
            guigu::core::event::AgentEvent::ToolExecutionEnd {
                tool_call_id,
                is_error,
                ..
            } => Some((tool_call_id.clone(), *is_error)),
            _ => None,
        })
        .collect();
    assert_eq!(
        tool_events,
        vec![
            ("c1".to_string(), false), // Start c1
            ("c1".to_string(), true),  // End c1（is_error）
            ("c2".to_string(), false), // Start c2
            ("c2".to_string(), true),  // End c2（is_error）
        ],
        "each tool call should emit Start then End(is_error=true) in input order"
    );

    // 3. 两个合成 ToolResult 均入 transcript，is_error 且携带截断消息，顺序 c1→c2。
    let snapshot = handle.snapshot();
    // user + assistant(toolcall, Length) + toolresult(c1) + toolresult(c2) + assistant(text)
    assert_eq!(snapshot.messages.len(), 5, "expected 5 messages");
    let tool_results: Vec<&guigu::core::message::ToolResultMessage> = snapshot
        .messages
        .iter()
        .filter_map(|m| match m.as_ref() {
            Message::ToolResult(tr) => Some(tr),
            _ => None,
        })
        .collect();
    assert_eq!(tool_results.len(), 2, "two synthesized tool results");
    assert_eq!(tool_results[0].tool_call_id, "c1", "first result is c1");
    assert_eq!(tool_results[1].tool_call_id, "c2", "second result is c2");
    for (i, tr) in tool_results.iter().enumerate() {
        assert!(
            tr.is_error,
            "synthesized tool result {i} should be an error"
        );
        let text = tr.content.iter().find_map(|c| match c {
            guigu::core::message::ToolResultContent::Text { text } => Some(text.clone()),
            _ => None,
        });
        assert_eq!(
            text.as_deref(),
            Some("tool call arguments truncated by length limit"),
            "synthesized tool result {i} should carry the truncation message"
        );
    }
}

/// Length 保护负例：stop_reason == Completed 且含 ToolCall → 正常执行工具（行为不变）。
#[tokio::test]
async fn test_length_negative_completed_executes() {
    let provider = FakeProvider::new(vec![
        tool_call_turn_with_stop("c1", "seq", "{}", StopReason::Completed),
        text_turn("done"),
    ]);
    let counter = Arc::new(AtomicUsize::new(0));
    let tools = vec![Arc::new(SeqTool {
        name: "seq".to_string(),
        counter: counter.clone(),
    }) as Arc<dyn Tool>];
    let handle = AgentHandle::spawn(
        make_config(),
        make_runtime(provider.clone(), tools, ToolExecutionMode::Sequential, 8192),
    );
    handle
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");
    handle.wait_for_idle().await.expect("should settle");

    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "Completed stop_reason should execute the tool normally"
    );
}

/// Length 且无 ToolCall → 正常结束，无合成 ToolResult（仅截断文本，合法终态）。
#[tokio::test]
async fn test_length_without_tool_calls() {
    let message = AssistantMessage {
        content: vec![AssistantContent::Text {
            text: "truncated".to_string(),
        }],
        model: None,
        usage: None,
        stop_reason: Some(StopReason::Length),
        error_message: None,
        timestamp: 0,
    };
    let events = vec![
        AssistantEvent::TextDelta {
            text: "truncated".to_string(),
        },
        AssistantEvent::Done { message },
    ];
    let provider = FakeProvider::new(vec![events]);
    let handle = AgentHandle::spawn(
        make_config(),
        make_runtime(
            provider.clone(),
            Vec::new(),
            ToolExecutionMode::Sequential,
            8192,
        ),
    );
    handle
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");
    handle.wait_for_idle().await.expect("should settle");

    let snapshot = handle.snapshot();
    // user + assistant(text, Length) —— 无合成 ToolResult。
    assert_eq!(
        snapshot.messages.len(),
        2,
        "expected 2 messages (no synthesized ToolResult)"
    );
    let last = snapshot
        .messages
        .last()
        .expect("transcript should not be empty");
    let Message::Assistant(a) = last.as_ref() else {
        panic!("last message should be assistant");
    };
    assert_eq!(
        a.stop_reason,
        Some(StopReason::Length),
        "should preserve Length stop_reason"
    );
}

/// 建流取消：provider 的 stream() 挂起（pending future）→ 取消 signal 后
/// 建流返回 Aborted，不进入重试（call_count == 1），run 产出 Aborted 终态。
#[tokio::test]
async fn test_stream_establishment_cancel() {
    let provider = Arc::new(HangingProvider {
        call_count: AtomicUsize::new(0),
    });
    let handle = AgentHandle::spawn(
        make_config(),
        make_runtime(
            provider.clone(),
            Vec::new(),
            ToolExecutionMode::Sequential,
            8192,
        ),
    );
    let mut rx = handle.subscribe();
    handle
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");
    // 等 run 进入建流（AgentStart 在建流前发出）。
    wait_event(&mut rx, |e| {
        matches!(e, guigu::core::event::AgentEvent::AgentStart)
    })
    .await
    .expect("should receive AgentStart");
    // 取消 run signal（shutdown 直接 cancel shutdown_token → run 级 child signal）。
    handle.shutdown().await.expect("shutdown should succeed");
    // 建流被取消：不进入重试，provider.stream() 仅调用一次。
    assert_eq!(
        provider.call_count(),
        1,
        "Aborted should not retry (stream called once)"
    );
    // run 产出 Aborted 终态（AgentEnd 携带 transcript）。
    let end = wait_event(&mut rx, |e| {
        matches!(e, guigu::core::event::AgentEvent::AgentEnd { .. })
    })
    .await
    .expect("AgentEnd should be delivered");
    let guigu::core::event::AgentEvent::AgentEnd { messages } = end else {
        panic!("expected AgentEnd");
    };
    let last = messages.last().expect("transcript should not be empty");
    let Message::Assistant(a) = last.as_ref() else {
        panic!("last message should be assistant");
    };
    assert_eq!(
        a.stop_reason,
        Some(StopReason::Aborted),
        "cancelled stream establishment should produce Aborted"
    );
}

/// 建流超时：request_timeout = Some(small) + 挂起 provider → 建流超时（可重试），
/// 重试耗尽后 run 产出 Error 终态（error_message 含 timeout）。
#[tokio::test]
async fn test_stream_establishment_timeout() {
    let provider = Arc::new(HangingProvider {
        call_count: AtomicUsize::new(0),
    });
    let runtime = AgentRuntime {
        provider: provider.clone(),
        tools: Vec::new(),
        loop_config: LoopConfig {
            model: Model {
                id: "test-model".to_string(),
                context_window: 8192,
            },
            request_timeout: Some(Duration::from_millis(50)),
            max_retries: 2,
            retry_base_delay: Duration::from_millis(1),
            ..LoopConfig::default()
        },
    };
    let handle = AgentHandle::spawn(make_config(), runtime);
    handle
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");
    handle.wait_for_idle().await.expect("should settle");

    // Timeout 可重试：2 次重试 + 1 次首次 = 3 次建流调用。
    assert_eq!(
        provider.call_count(),
        3,
        "timeout should be retried (2 retries + 1 initial = 3 calls)"
    );
    let snapshot = handle.snapshot();
    let last = snapshot
        .messages
        .last()
        .expect("transcript should not be empty");
    let Message::Assistant(a) = last.as_ref() else {
        panic!("last message should be assistant");
    };
    assert_eq!(
        a.stop_reason,
        Some(StopReason::Error),
        "timeout (retries exhausted) should produce Error"
    );
    assert!(a.error_message.is_some(), "should carry an error message");
}

/// 从 broadcast 接收事件直到 `AgentEnd`（含），带 5s 超时兜底。
/// 用于断言逐调用生命周期事件的完整序列（Start/End 顺序 + is_error）。
async fn collect_until_agent_end(
    rx: &mut tokio::sync::broadcast::Receiver<guigu::core::event::AgentEvent>,
) -> Vec<guigu::core::event::AgentEvent> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut events = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!("collect_until_agent_end: timeout before AgentEnd");
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(event)) => {
                let is_end = matches!(event, guigu::core::event::AgentEvent::AgentEnd { .. });
                events.push(event);
                if is_end {
                    return events;
                }
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                panic!("collect_until_agent_end: event channel closed before AgentEnd");
            }
            Err(_) => panic!("collect_until_agent_end: timeout before AgentEnd"),
        }
    }
}

/// 从 broadcast 接收事件直到匹配 predicate，带 5s 超时兜底。
async fn wait_event(
    rx: &mut tokio::sync::broadcast::Receiver<guigu::core::event::AgentEvent>,
    mut predicate: impl FnMut(&guigu::core::event::AgentEvent) -> bool,
) -> Result<guigu::core::event::AgentEvent, String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err("wait_event timeout".to_string());
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(event)) => {
                if predicate(&event) {
                    return Ok(event);
                }
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                return Err("event channel closed".to_string());
            }
            Err(_) => return Err("wait_event timeout".to_string()),
        }
    }
}
