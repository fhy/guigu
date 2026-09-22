//! Runtime loop 场景测试。
mod common;
use common::*;
use guigu::core::message::Message;
use guigu::core::provider::ProviderError;
use guigu::core::{AgentHandle, ToolExecutionMode};

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
