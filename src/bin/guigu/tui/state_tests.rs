//! TUI 状态映射单测（Task 023）：`apply_event` 事件序列 → 状态断言。
//!
//! 从 `state.rs` 拆出（单文件 ≤ 400 行约束），经 `#[path]` 挂为 `state::tests`。
//! 纯逻辑（无终端/IO），喂事件序列断言最终状态。

use super::*;
use std::sync::Arc;

/// 构造一条 user 消息事件。
fn user_event(text: &str) -> AgentEvent {
    AgentEvent::MessageStart {
        message: Arc::new(Message::User(UserMessage {
            content: vec![UserContent::Text {
                text: text.to_string(),
            }],
            timestamp: 0,
        })),
    }
}

/// 构造一条 assistant 文本增量事件。
fn text_delta(delta: &str) -> AgentEvent {
    AgentEvent::MessageUpdate {
        message: Arc::new(Message::Assistant(guigu::core::message::AssistantMessage {
            content: Vec::new(),
            model: None,
            usage: None,
            stop_reason: None,
            error_message: None,
            timestamp: 0,
        })),
        assistant_event: AssistantEvent::TextDelta {
            text: delta.to_string(),
        },
    }
}

/// 构造一条 assistant 消息结束事件（可带 usage）。
fn assistant_end(usage: Option<Usage>) -> AgentEvent {
    AgentEvent::MessageEnd {
        message: Arc::new(Message::Assistant(guigu::core::message::AssistantMessage {
            content: Vec::new(),
            model: None,
            usage,
            stop_reason: None,
            error_message: None,
            timestamp: 0,
        })),
    }
}

/// 构造一条工具执行开始事件。
fn tool_start(id: &str, name: &str) -> AgentEvent {
    AgentEvent::ToolExecutionStart {
        tool_call_id: id.to_string(),
        tool_name: name.to_string(),
        args: serde_json::json!({"cmd": "ls"}),
    }
}

/// 构造一条工具执行结束事件。
fn tool_end(id: &str, text: &str, is_error: bool) -> AgentEvent {
    AgentEvent::ToolExecutionEnd {
        tool_call_id: id.to_string(),
        tool_name: "bash".to_string(),
        result: ToolResult::text(text),
        is_error,
    }
}

/// 取对话区第 i 个工具卡片。
fn tool_card_at(state: &TuiState, i: usize) -> &ToolCard {
    match &state.conv[i] {
        ConvItem::Tool(card) => card,
        other => panic!("expected tool card, got {other:?}"),
    }
}

/// 构造一条携带任意 `AssistantEvent` 的 `MessageUpdate` 事件。
fn assistant_update(event: AssistantEvent) -> AgentEvent {
    AgentEvent::MessageUpdate {
        message: Arc::new(Message::Assistant(guigu::core::message::AssistantMessage {
            content: Vec::new(),
            model: None,
            usage: None,
            stop_reason: None,
            error_message: None,
            timestamp: 0,
        })),
        assistant_event: event,
    }
}

#[test]
fn user_message_appends_bubble() {
    let mut state = TuiState::new("m".into(), "l".into());
    apply_event(&mut state, &user_event("hello"));
    assert_eq!(
        state.conv,
        vec![ConvItem::User {
            text: "hello".into()
        }]
    );
}

#[test]
fn text_delta_accumulates_into_streaming() {
    let mut state = TuiState::new("m".into(), "l".into());
    apply_event(&mut state, &text_delta("Hel"));
    apply_event(&mut state, &text_delta("lo"));
    assert_eq!(
        state.streaming.as_ref().map(|s| s.text.as_str()),
        Some("Hello")
    );
    // 未落定前对话区无 assistant 气泡。
    assert!(state.conv.is_empty());
}

#[test]
fn assistant_end_finalizes_streaming_and_records_usage() {
    let mut state = TuiState::new("m".into(), "l".into());
    apply_event(&mut state, &text_delta("done"));
    let usage = Usage {
        input: 10,
        output: 5,
        cache_read: 0,
        cache_write: 0,
        total_tokens: 15,
        cost: 0.0,
    };
    apply_event(&mut state, &assistant_end(Some(usage.clone())));
    assert_eq!(
        state.conv,
        vec![ConvItem::Assistant {
            text: "done".into()
        }]
    );
    assert!(state.streaming.is_none());
    assert_eq!(state.usage, Some(usage));
}

#[test]
fn assistant_end_without_text_adds_no_bubble() {
    let mut state = TuiState::new("m".into(), "l".into());
    // 仅工具调用、无文本：不落定空气泡。
    apply_event(&mut state, &assistant_end(None));
    assert!(state.conv.is_empty());
}

#[test]
fn tool_card_state_machine_running_to_done() {
    let mut state = TuiState::new("m".into(), "l".into());
    apply_event(&mut state, &tool_start("t1", "bash"));
    assert_eq!(tool_card_at(&state, 0).status, ToolCardStatus::Running);
    assert_eq!(tool_card_at(&state, 0).name, "bash");

    apply_event(&mut state, &tool_end("t1", "file.txt", false));
    assert_eq!(tool_card_at(&state, 0).status, ToolCardStatus::Done);
    assert_eq!(tool_card_at(&state, 0).output, "file.txt");
}

#[test]
fn tool_card_failed_on_error() {
    let mut state = TuiState::new("m".into(), "l".into());
    apply_event(&mut state, &tool_start("t1", "bash"));
    apply_event(&mut state, &tool_end("t1", "boom", true));
    assert_eq!(tool_card_at(&state, 0).status, ToolCardStatus::Failed);
}

#[test]
fn tool_call_start_creates_card_before_execution() {
    let mut state = TuiState::new("m".into(), "l".into());
    let ev = AgentEvent::MessageUpdate {
        message: Arc::new(Message::Assistant(guigu::core::message::AssistantMessage {
            content: Vec::new(),
            model: None,
            usage: None,
            stop_reason: None,
            error_message: None,
            timestamp: 0,
        })),
        assistant_event: AssistantEvent::ToolCallStart {
            id: "t9".into(),
            name: "read".into(),
            arguments: "{}".into(),
        },
    };
    apply_event(&mut state, &ev);
    assert_eq!(tool_card_at(&state, 0).id, "t9");
    assert_eq!(tool_card_at(&state, 0).name, "read");
    assert_eq!(tool_card_at(&state, 0).status, ToolCardStatus::Running);
}

#[test]
fn tool_call_delta_accumulates_chunked_args() {
    let mut state = TuiState::new("m".into(), "l".into());
    // ToolCallStart 空参数（流式 provider 先开卡片再分片发参数）。
    apply_event(
        &mut state,
        &assistant_update(AssistantEvent::ToolCallStart {
            id: "t1".into(),
            name: "bash".into(),
            arguments: String::new(),
        }),
    );
    assert_eq!(tool_card_at(&state, 0).args, "");

    // 分片参数 delta 逐段累积。
    for delta in ["{\"cmd\": ", "ls\"}"] {
        apply_event(
            &mut state,
            &assistant_update(AssistantEvent::ToolCallDelta {
                id: "t1".into(),
                arguments_delta: delta.into(),
            }),
        );
    }
    assert_eq!(tool_card_at(&state, 0).args, "{\"cmd\": ls\"}");

    // ToolCallEnd 不改变卡片（args 已累积完整，状态仍 Running）。
    apply_event(
        &mut state,
        &assistant_update(AssistantEvent::ToolCallEnd { id: "t1".into() }),
    );
    assert_eq!(tool_card_at(&state, 0).args, "{\"cmd\": ls\"}");
    assert_eq!(tool_card_at(&state, 0).status, ToolCardStatus::Running);

    // ToolExecutionStart 携带完整 args，覆盖累积值。
    apply_event(&mut state, &tool_start("t1", "bash"));
    assert_eq!(tool_card_at(&state, 0).args, "{\"cmd\":\"ls\"}");
}

#[test]
fn agent_end_sets_idle_and_finalizes_residual() {
    let mut state = TuiState::new("m".into(), "l".into());
    apply_event(&mut state, &AgentEvent::AgentStart);
    assert_eq!(state.status, Status::Running);
    apply_event(&mut state, &text_delta("tail"));
    apply_event(
        &mut state,
        &AgentEvent::AgentEnd {
            messages: Vec::new(),
        },
    );
    assert_eq!(state.status, Status::Idle);
    assert_eq!(
        state.conv,
        vec![ConvItem::Assistant {
            text: "tail".into()
        }]
    );
}

#[test]
fn assistant_error_sets_error_status() {
    let mut state = TuiState::new("m".into(), "l".into());
    let ev = AgentEvent::MessageUpdate {
        message: Arc::new(Message::Assistant(guigu::core::message::AssistantMessage {
            content: Vec::new(),
            model: None,
            usage: None,
            stop_reason: None,
            error_message: None,
            timestamp: 0,
        })),
        assistant_event: AssistantEvent::Error {
            message: "HTTP 500".into(),
            aborted: false,
        },
    };
    apply_event(&mut state, &ev);
    assert_eq!(state.status, Status::Error);
    assert_eq!(state.error.as_deref(), Some("HTTP 500"));
}

#[test]
fn thinking_delta_accumulates_collapsed() {
    let mut state = TuiState::new("m".into(), "l".into());
    let ev = AgentEvent::MessageUpdate {
        message: Arc::new(Message::Assistant(guigu::core::message::AssistantMessage {
            content: Vec::new(),
            model: None,
            usage: None,
            stop_reason: None,
            error_message: None,
            timestamp: 0,
        })),
        assistant_event: AssistantEvent::ThinkingDelta {
            thinking: "hmm".into(),
        },
    };
    apply_event(&mut state, &ev);
    assert_eq!(
        state.streaming.as_ref().map(|s| s.thinking.as_str()),
        Some("hmm")
    );
}

#[test]
fn truncate_caps_chars_and_marks_ellipsis() {
    let long = "a".repeat(500);
    let t = truncate(&long, 400);
    assert_eq!(t.chars().count(), 401); // 400 + '…'
    assert!(t.ends_with('…'));
    assert_eq!(truncate("short", 400), "short");
}
