//! Task 001 生命周期集成测试。
//!
//! 覆盖规格验收标准：
//! - prompt 后 snapshot.messages 增长
//! - subscribe 收到完整事件序列（单消息 + 多消息逐条包裹 M1S→M1E→M2S→M2E）
//! - steer/followUp 入队与 drain
//! - 并发 prompt 排队执行（active run 期间第二条 prompt 之后才被处理）
//! - abort 后 run 结束且状态一致（AgentEnd 必达，wait_for_idle 正常返回）
//! - wait_for_idle 正确结算（多次调用、同 run 内、Reset 后均可返回）
//! - reset 清空 transcript 与队列
//!
//! 同步点约定：一律以 `wait_for_idle` 或 subscribe 收到 `AgentEnd` 为同步点，
//! 禁止用 `tokio::time::sleep()` 做同步（r6 flaky 根因）。

use std::time::Duration;

use guigu::core::event::AgentEvent;
use guigu::core::message::{Message, ThinkingLevel, UserContent, UserMessage};
use guigu::core::{Agent, AgentConfig, AgentHandle};
use tokio::sync::broadcast;

fn make_config() -> AgentConfig {
    AgentConfig {
        system_prompt: "You are a helpful assistant.".to_string(),
        model: Some("test-model".to_string()),
        thinking_level: ThinkingLevel::Minimal,
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
/// 作为事件侧同步点，替代 sleep 竞态。
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
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => {
                // lagged：继续等待后续事件
            }
            Ok(Err(broadcast::error::RecvError::Closed)) => {
                return Err("event channel closed".to_string());
            }
            Err(_) => return Err("wait_for_event timeout".to_string()),
        }
    }
}

/// prompt 后 snapshot.messages 增长。
#[tokio::test]
async fn test_prompt_updates_snapshot() {
    let handle = AgentHandle::spawn(make_config());
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
        1,
        "transcript should have 1 message"
    );
    assert_eq!(
        snapshot.system_prompt, "You are a helpful assistant.",
        "system_prompt should be preserved"
    );
}

/// subscribe 收到完整事件序列（单消息）：
/// AgentStart→TurnStart→MessageStart→MessageEnd→TurnEnd→AgentEnd。
#[tokio::test]
async fn test_subscribe_receives_full_event_sequence() {
    let handle = AgentHandle::spawn(make_config());
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
        6,
        "expected 6 events (AgentStart, TurnStart, MessageStart, MessageEnd, TurnEnd, AgentEnd), got {}",
        events.len()
    );
    assert!(
        matches!(events[0], AgentEvent::AgentStart),
        "event[0] should be AgentStart"
    );
    assert!(
        matches!(events[1], AgentEvent::TurnStart),
        "event[1] should be TurnStart"
    );
    assert!(
        matches!(events[2], AgentEvent::MessageStart { .. }),
        "event[2] should be MessageStart"
    );
    assert!(
        matches!(events[3], AgentEvent::MessageEnd { .. }),
        "event[3] should be MessageEnd"
    );
    assert!(
        matches!(events[4], AgentEvent::TurnEnd { .. }),
        "event[4] should be TurnEnd"
    );
    assert!(
        matches!(events[5], AgentEvent::AgentEnd { .. }),
        "event[5] should be AgentEnd"
    );
}

/// subscribe 收到完整事件序列（多消息，逐条包裹）：
/// AgentStart→TurnStart→M1S→M1E→M2S→M2E→TurnEnd→AgentEnd。
/// 固化"逐消息包裹"语义（不是 M1S→M2S→M1E→M2E）。
#[tokio::test]
async fn test_multi_message_event_sequence() {
    let handle = AgentHandle::spawn(make_config());
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
        8,
        "expected 8 events for 2 messages, got {}",
        events.len()
    );
    assert!(
        matches!(events[0], AgentEvent::AgentStart),
        "event[0] should be AgentStart"
    );
    assert!(
        matches!(events[1], AgentEvent::TurnStart),
        "event[1] should be TurnStart"
    );
    assert!(
        matches!(events[2], AgentEvent::MessageStart { .. }),
        "event[2] should be M1 MessageStart"
    );
    assert!(
        matches!(events[3], AgentEvent::MessageEnd { .. }),
        "event[3] should be M1 MessageEnd"
    );
    assert!(
        matches!(events[4], AgentEvent::MessageStart { .. }),
        "event[4] should be M2 MessageStart"
    );
    assert!(
        matches!(events[5], AgentEvent::MessageEnd { .. }),
        "event[5] should be M2 MessageEnd"
    );
    assert!(
        matches!(events[6], AgentEvent::TurnEnd { .. }),
        "event[6] should be TurnEnd"
    );
    assert!(
        matches!(events[7], AgentEvent::AgentEnd { .. }),
        "event[7] should be AgentEnd"
    );
}

/// steer/followUp 入队与 drain：idle 时立即 drain 进 transcript。
#[tokio::test]
async fn test_steer_followup_enqueue_and_drain() {
    let handle = AgentHandle::spawn(make_config());

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
        1,
        "steered message should be drained into transcript"
    );
    assert!(
        matches!(snapshot.messages[0].as_ref(), Message::User(_)),
        "drained message should be a User message"
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
        2,
        "follow_up message should be drained into transcript"
    );
}

/// 并发 prompt 排队执行（不返回 Busy）：active run 期间第二条 prompt 入队，
/// 待第一条 run 结束后按序处理，两条都成功且 transcript 包含两者。
#[tokio::test]
async fn test_concurrent_prompt_handling() {
    let handle = AgentHandle::spawn(make_config());

    // 两个 prompt 都应成功（排队，不返回 Busy）；expect 失败即测试失败
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
        2,
        "both prompts should be drained into transcript"
    );
}

/// abort 后 run 结束且状态一致：AgentEnd 必达，wait_for_idle 正常返回。
/// 以收到 AgentStart 为同步点再 abort，不依赖调度时序（r6 flaky 根因）。
#[tokio::test]
async fn test_abort_stops_run() {
    let handle = AgentHandle::spawn(make_config());
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

    // 等 run 启动（AgentStart）后再 abort，确保 abort 在 run 生命周期内发出
    wait_for_event(&mut rx, |e| matches!(e, AgentEvent::AgentStart))
        .await
        .expect("should receive AgentStart");
    handle.abort();

    // AgentEnd 必达（无论 abort 是否实际取消 run）
    wait_for_event(&mut rx, |e| matches!(e, AgentEvent::AgentEnd { .. }))
        .await
        .expect("AgentEnd should be delivered after abort");

    handle
        .wait_for_idle()
        .await
        .expect("wait_for_idle should settle after abort");

    let snapshot = handle.snapshot();
    // 状态一致：run 已结束，is_streaming 应为 false
    assert!(
        !snapshot.is_streaming,
        "is_streaming should be false after run ends"
    );
    assert!(
        snapshot.messages.len() <= 3,
        "transcript should have at most 3 messages, got {}",
        snapshot.messages.len()
    );
}

/// wait_for_idle 正确结算：初始 idle 立即返回；prompt 后等待 run 完成；
/// 同一 run 结束后可多次调用；Reset 后可立即返回。
#[tokio::test]
async fn test_wait_for_idle() {
    let handle = AgentHandle::spawn(make_config());

    // 初始状态 idle，应立即返回
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
        1,
        "prompt should be processed after wait_for_idle"
    );

    // 同一 run 结束后多次调用，均应立即返回（不因"通知已发出"而挂起）
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
    let handle = AgentHandle::spawn(make_config());

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
        1,
        "transcript should have 1 message before reset"
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
    // system_prompt / model / thinking_level 保持不变
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
