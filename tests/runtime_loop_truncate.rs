//! Runtime loop 场景测试。
mod common;
use common::*;
use guigu::Agent;
use guigu::core::message::{Message, StopReason};
use guigu::core::tool::Tool;
use guigu::core::{AgentHandle, ToolExecutionMode};
use std::sync::Arc;

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
