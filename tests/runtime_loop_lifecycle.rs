//! Runtime loop 场景测试。
mod common;
use common::*;
use guigu::core::{AgentHandle, ToolExecutionMode};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

/// 建流取消：provider 的 stream() 挂起（pending future）后取消 run signal。
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
