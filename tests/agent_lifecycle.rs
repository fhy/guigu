//! Task 001 生命周期集成测试（003 接入执行引擎后更新）。
//!
//! 覆盖规格验收标准：
//! - prompt 后 snapshot.messages 增长（user + assistant）
//! - subscribe 收到完整事件序列（逐消息包裹 + assistant 流式事件）
//! - steer/followUp 驱动 run
//! - 并发 prompt 排队执行（active run 期间第二条 prompt 之后才被处理）
//! - abort 后 run 结束且状态一致（AgentEnd 必达，wait_for_idle 正常返回）
//! - wait_for_idle 正确结算（多次调用、同 run 内、Reset 后均可返回）
//! - reset 清空 transcript 与队列
//!
//! 同步点约定：一律以 `wait_for_idle` 或 subscribe 收到 `AgentEnd` 为同步点，
//! 禁止用 `tokio::time::sleep()` 做同步（r6 flaky 根因）。
//!
//! 003 起 runtime 接入真实 loop：prompt 会触发 provider 调用，transcript 含
//! user + assistant 消息，事件序列含 assistant 的 MessageStart/Update/End。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use guigu::core::event::AgentEvent;
use guigu::core::message::{
    AssistantContent, AssistantMessage, Message, ModelId, StopReason, ThinkingLevel, UserContent,
    UserMessage,
};
use guigu::core::provider::{
    AssistantEvent, AssistantStream, ModelProvider, ProviderError, ProviderRequest,
};
use guigu::core::{Agent, AgentConfig, AgentHandle, AgentRuntime, LoopConfig, Model};
use tokio::sync::broadcast;

/// 最小文本 provider：回显固定文本（一个 TextDelta + Done）。
struct TextProvider {
    text: String,
}

#[async_trait]
impl ModelProvider for TextProvider {
    async fn stream(&self, request: ProviderRequest) -> Result<AssistantStream, ProviderError> {
        let message = AssistantMessage {
            content: vec![AssistantContent::Text {
                text: self.text.clone(),
            }],
            model: Some(ModelId(request.model.id.clone())),
            usage: None,
            stop_reason: Some(StopReason::Completed),
            error_message: None,
            timestamp: 0,
        };
        let events = vec![
            AssistantEvent::TextDelta {
                text: self.text.clone(),
            },
            AssistantEvent::Done { message },
        ];
        Ok(Box::pin(stream::iter(events)))
    }
}

fn make_config() -> AgentConfig {
    AgentConfig {
        system_prompt: "You are a helpful assistant.".to_string(),
        model: Some("test-model".to_string()),
        thinking_level: ThinkingLevel::Minimal,
    }
}

fn make_runtime() -> AgentRuntime {
    AgentRuntime {
        provider: Arc::new(TextProvider {
            text: "ok".to_string(),
        }),
        tools: Vec::new(),
        loop_config: LoopConfig {
            model: Model {
                id: "test-model".to_string(),
                context_window: 8192,
            },
            ..LoopConfig::default()
        },
    }
}

fn make_user_message(text: &str) -> Message {
    Message::User(UserMessage {
        content: vec![UserContent::Text {
            text: text.to_string(),
        }],
        timestamp: 0,
    })
}

/// 从 broadcast 接收事件直到匹配 predicate，带 5s 超时兜底。
async fn wait_for_event(
    rx: &mut broadcast::Receiver<AgentEvent>,
    mut predicate: impl FnMut(&AgentEvent) -> bool,
) -> Result<AgentEvent, String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err("wait_for_event timeout".to_string());
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(event)) => {
                if predicate(&event) {
                    return Ok(event);
                }
            }
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(broadcast::error::RecvError::Closed)) => {
                return Err("event channel closed".to_string());
            }
            Err(_) => return Err("wait_for_event timeout".to_string()),
        }
    }
}

/// prompt 后 snapshot.messages 增长（user + assistant）。
#[tokio::test]
async fn test_prompt_updates_snapshot() {
    let handle = AgentHandle::spawn(make_config(), make_runtime());
    handle
        .prompt(vec![make_user_message("Hello")])
        .await
        .expect("prompt should succeed");
    handle
        .wait_for_idle()
        .await
        .expect("wait_for_idle should settle");
    let snapshot = handle.snapshot();
    assert_eq!(
        snapshot.messages.len(),
        2,
        "transcript should have user + assistant messages"
    );
    assert_eq!(
        snapshot.system_prompt, "You are a helpful assistant.",
        "system_prompt should be preserved"
    );
}

/// subscribe 收到完整事件序列（单消息）：
/// MessageStart(user)→MessageEnd(user)→AgentStart→TurnStart
/// →MessageStart(assistant)→MessageUpdate→MessageEnd(assistant)→TurnEnd→AgentEnd。
#[tokio::test]
async fn test_subscribe_receives_full_event_sequence() {
    let handle = AgentHandle::spawn(make_config(), make_runtime());
    let mut rx = handle.subscribe();
    handle
        .prompt(vec![make_user_message("Hello")])
        .await
        .expect("prompt should succeed");
    handle
        .wait_for_idle()
        .await
        .expect("wait_for_idle should settle");

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }

    assert_eq!(
        events.len(),
        9,
        "expected 9 events, got {}: {:?}",
        events.len(),
        events
    );
    assert!(
        matches!(events[0], AgentEvent::MessageStart { .. }),
        "event[0] should be MessageStart(user)"
    );
    assert!(
        matches!(events[1], AgentEvent::MessageEnd { .. }),
        "event[1] should be MessageEnd(user)"
    );
    assert!(
        matches!(events[2], AgentEvent::AgentStart),
        "event[2] should be AgentStart"
    );
    assert!(
        matches!(events[3], AgentEvent::TurnStart),
        "event[3] should be TurnStart"
    );
    assert!(
        matches!(events[4], AgentEvent::MessageStart { .. }),
        "event[4] should be MessageStart(assistant)"
    );
    assert!(
        matches!(events[5], AgentEvent::MessageUpdate { .. }),
        "event[5] should be MessageUpdate"
    );
    assert!(
        matches!(events[6], AgentEvent::MessageEnd { .. }),
        "event[6] should be MessageEnd(assistant)"
    );
    assert!(
        matches!(events[7], AgentEvent::TurnEnd { .. }),
        "event[7] should be TurnEnd"
    );
    assert!(
        matches!(events[8], AgentEvent::AgentEnd { .. }),
        "event[8] should be AgentEnd"
    );
}

/// subscribe 收到完整事件序列（多消息，逐条包裹）：
/// M1S→M1E→M2S→M2E→AgentStart→TurnStart→AS→AU→AE→TurnEnd→AgentEnd。
#[tokio::test]
async fn test_multi_message_event_sequence() {
    let handle = AgentHandle::spawn(make_config(), make_runtime());
    let mut rx = handle.subscribe();
    handle
        .prompt(vec![make_user_message("M1"), make_user_message("M2")])
        .await
        .expect("prompt should succeed");
    handle
        .wait_for_idle()
        .await
        .expect("wait_for_idle should settle");

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }

    assert_eq!(
        events.len(),
        11,
        "expected 11 events for 2 messages, got {}: {:?}",
        events.len(),
        events
    );
    assert!(
        matches!(events[0], AgentEvent::MessageStart { .. }),
        "event[0] should be M1 MessageStart"
    );
    assert!(
        matches!(events[1], AgentEvent::MessageEnd { .. }),
        "event[1] should be M1 MessageEnd"
    );
    assert!(
        matches!(events[2], AgentEvent::MessageStart { .. }),
        "event[2] should be M2 MessageStart"
    );
    assert!(
        matches!(events[3], AgentEvent::MessageEnd { .. }),
        "event[3] should be M2 MessageEnd"
    );
    assert!(
        matches!(events[4], AgentEvent::AgentStart),
        "event[4] should be AgentStart"
    );
    assert!(
        matches!(events[10], AgentEvent::AgentEnd { .. }),
        "event[10] should be AgentEnd"
    );
}

/// steer/followUp 驱动 run：idle 时 steer 立即触发 run（user + assistant）。
#[tokio::test]
async fn test_steer_followup_enqueue_and_drain() {
    let handle = AgentHandle::spawn(make_config(), make_runtime());

    handle
        .steer(make_user_message("Steered"))
        .await
        .expect("steer should succeed");
    handle
        .wait_for_idle()
        .await
        .expect("wait_for_idle should settle");
    let snapshot = handle.snapshot();
    assert_eq!(
        snapshot.messages.len(),
        2,
        "steer should produce user + assistant messages"
    );
    assert!(
        matches!(snapshot.messages[0].as_ref(), Message::User(_)),
        "first message should be a User message"
    );

    handle
        .follow_up(make_user_message("Followed up"))
        .await
        .expect("follow_up should succeed");
    handle
        .wait_for_idle()
        .await
        .expect("wait_for_idle should settle");
    let snapshot = handle.snapshot();
    assert_eq!(
        snapshot.messages.len(),
        4,
        "follow_up should add another user + assistant pair"
    );
}

/// 并发 prompt 排队执行（不返回 Busy）：active run 期间第二条 prompt 入队，
/// 待第一条 run 结束后按序处理，两条都成功且 transcript 包含两者。
#[tokio::test]
async fn test_concurrent_prompt_handling() {
    let handle = AgentHandle::spawn(make_config(), make_runtime());

    handle
        .prompt(vec![make_user_message("First")])
        .await
        .expect("first prompt should succeed (queued, not Busy)");
    handle
        .prompt(vec![make_user_message("Second")])
        .await
        .expect("second prompt should succeed (queued, not Busy)");

    handle
        .wait_for_idle()
        .await
        .expect("wait_for_idle should settle");

    let snapshot = handle.snapshot();
    assert_eq!(
        snapshot.messages.len(),
        4,
        "both prompts should produce user + assistant pairs"
    );
}

/// abort 后 run 结束且状态一致：AgentEnd 必达，wait_for_idle 正常返回。
#[tokio::test]
async fn test_abort_stops_run() {
    let handle = AgentHandle::spawn(make_config(), make_runtime());
    let mut rx = handle.subscribe();

    let messages = vec![
        make_user_message("Msg1"),
        make_user_message("Msg2"),
        make_user_message("Msg3"),
    ];
    handle
        .prompt(messages)
        .await
        .expect("prompt should succeed");

    wait_for_event(&mut rx, |e| matches!(e, AgentEvent::AgentStart))
        .await
        .expect("should receive AgentStart");
    handle.abort();

    wait_for_event(&mut rx, |e| matches!(e, AgentEvent::AgentEnd { .. }))
        .await
        .expect("AgentEnd should be delivered after abort");

    handle
        .wait_for_idle()
        .await
        .expect("wait_for_idle should settle after abort");

    let snapshot = handle.snapshot();
    assert!(
        !snapshot.is_streaming,
        "is_streaming should be false after run ends"
    );
    assert!(
        snapshot.messages.len() <= 4,
        "transcript should have at most 4 messages, got {}",
        snapshot.messages.len()
    );
}

/// wait_for_idle 正确结算：初始 idle 立即返回；prompt 后等待 run 完成；
/// 同一 run 结束后可多次调用；Reset 后可立即返回。
#[tokio::test]
async fn test_wait_for_idle() {
    let handle = AgentHandle::spawn(make_config(), make_runtime());

    handle
        .wait_for_idle()
        .await
        .expect("wait_for_idle should return immediately when idle");

    handle
        .prompt(vec![make_user_message("Hello")])
        .await
        .expect("prompt should succeed");
    handle
        .wait_for_idle()
        .await
        .expect("wait_for_idle should settle after prompt");

    let snapshot = handle.snapshot();
    assert_eq!(
        snapshot.messages.len(),
        2,
        "prompt should produce user + assistant after wait_for_idle"
    );

    handle
        .wait_for_idle()
        .await
        .expect("second wait_for_idle should return immediately");
    handle
        .wait_for_idle()
        .await
        .expect("third wait_for_idle should return immediately");
}

/// reset 清空 transcript 与队列：reset 后 transcript 为空，wait_for_idle 可立即返回。
#[tokio::test]
async fn test_reset_clears_transcript_and_queue() {
    let handle = AgentHandle::spawn(make_config(), make_runtime());

    handle
        .prompt(vec![make_user_message("Hello")])
        .await
        .expect("prompt should succeed");
    handle
        .wait_for_idle()
        .await
        .expect("wait_for_idle should settle");
    let snapshot = handle.snapshot();
    assert_eq!(
        snapshot.messages.len(),
        2,
        "prompt should produce user + assistant before reset"
    );

    handle.reset().await.expect("reset should succeed");
    handle
        .wait_for_idle()
        .await
        .expect("wait_for_idle should settle after reset");
    let snapshot = handle.snapshot();
    assert_eq!(
        snapshot.messages.len(),
        0,
        "transcript should be empty after reset"
    );
    assert_eq!(
        snapshot.system_prompt, "You are a helpful assistant.",
        "system_prompt should be preserved after reset"
    );
    assert_eq!(
        snapshot.model,
        Some("test-model".to_string()),
        "model should be preserved after reset"
    );
}
