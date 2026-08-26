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
        self.last_context_size
            .store(request.context.messages.len(), Ordering::SeqCst);
        let idx = self.call_index.fetch_add(1, Ordering::SeqCst);
        let events = self.turns.get(idx).cloned().unwrap_or_default();
        Ok(Box::pin(stream::iter(events)))
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
