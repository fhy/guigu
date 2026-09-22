//! Runtime loop 场景测试。
mod common;
use common::*;
use guigu::core::message::Message;
use guigu::core::tool::Tool;
use guigu::core::{AgentHandle, ToolExecutionMode};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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
