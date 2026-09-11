//! [`super::MergedHooks`] 单测：should_stop_after_turn / prepare_next_turn 合并
//! 语义（首个 Some(true) 即停 / 注入拼接 / 短路保留已成功部分）。
//!
//! 自 `hooks.rs` 拆出（单文件 ≤ 400 行约束，Task 029）。共享测试 helper 在
//! [`super::tests`]。

use super::tests::{NoopHooks, assistant, ctx, tool_result_msg};
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// MergedHooks should_stop_after_turn：首个 Some(true) 即停。
struct StopHooks {
    stop: bool,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LifecycleHooks for StopHooks {
    fn should_stop_after_turn(&self, _ctx: &HookContext) -> Option<bool> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Some(self.stop)
    }
}

/// MergedHooks should_stop_after_turn：a Some(false)、b Some(true) → 返回 Some(true)。
#[test]
fn test_merged_should_stop_first_true() {
    let calls_a = Arc::new(AtomicUsize::new(0));
    let calls_b = Arc::new(AtomicUsize::new(0));
    let merged = MergedHooks::new(vec![
        (
            "a".to_string(),
            Arc::new(StopHooks {
                stop: false,
                calls: calls_a.clone(),
            }) as Arc<dyn LifecycleHooks>,
        ),
        (
            "b".to_string(),
            Arc::new(StopHooks {
                stop: true,
                calls: calls_b.clone(),
            }) as Arc<dyn LifecycleHooks>,
        ),
    ]);
    assert_eq!(
        merged.should_stop_after_turn(&ctx()),
        Some(true),
        "should return Some(true) when b stops"
    );
    assert_eq!(calls_a.load(Ordering::SeqCst), 1);
    assert_eq!(calls_b.load(Ordering::SeqCst), 1);
}

/// MergedHooks should_stop_after_turn：全 None/Some(false) → 返回 None。
#[test]
fn test_merged_should_stop_all_none() {
    let merged = MergedHooks::new(vec![(
        "a".to_string(),
        Arc::new(NoopHooks) as Arc<dyn LifecycleHooks>,
    )]);
    assert_eq!(merged.should_stop_after_turn(&ctx()), None);
}

/// MergedHooks prepare_next_turn 拼接：a 注入 1 条、b 注入 1 条 → 共 2 条（按 id 序）。
struct InjectingHooksN {
    text: String,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LifecycleHooks for InjectingHooksN {
    async fn prepare_next_turn(
        &self,
        _ctx: &HookContext,
        _assistant: &AssistantMessage,
        _tool_results: &[ToolResultMessage],
    ) -> Result<Vec<Message>, HookError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![Message::User(crate::core::message::UserMessage {
            content: vec![crate::core::message::UserContent::Text {
                text: self.text.clone(),
            }],
            timestamp: 0,
        })])
    }
}

/// MergedHooks prepare_next_turn 拼接：a + b 各注入 1 条 → 共 2 条，按 id 序。
#[tokio::test]
async fn test_merged_prepare_next_turn_concat() {
    let calls_a = Arc::new(AtomicUsize::new(0));
    let calls_b = Arc::new(AtomicUsize::new(0));
    let merged = MergedHooks::new(vec![
        (
            "a".to_string(),
            Arc::new(InjectingHooksN {
                text: "from-a".to_string(),
                calls: calls_a.clone(),
            }) as Arc<dyn LifecycleHooks>,
        ),
        (
            "b".to_string(),
            Arc::new(InjectingHooksN {
                text: "from-b".to_string(),
                calls: calls_b.clone(),
            }) as Arc<dyn LifecycleHooks>,
        ),
    ]);
    let out = merged
        .prepare_next_turn(&ctx(), &assistant(), &[tool_result_msg()])
        .await
        .expect("should be Ok");
    assert_eq!(out.len(), 2, "should concatenate both injections");
    let texts: Vec<String> = out
        .iter()
        .filter_map(|m| match m {
            Message::User(u) => u.content.iter().find_map(|c| match c {
                crate::core::message::UserContent::Text { text } => Some(text.clone()),
                _ => None,
            }),
            _ => None,
        })
        .collect();
    assert_eq!(texts, vec!["from-a".to_string(), "from-b".to_string()]);
    assert_eq!(calls_a.load(Ordering::SeqCst), 1);
    assert_eq!(calls_b.load(Ordering::SeqCst), 1);
}

/// MergedHooks prepare_next_turn 短路：a 注入、b 拒绝 → 保留 a 的注入（已成功部分）。
struct PrepareRejectingHooks {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LifecycleHooks for PrepareRejectingHooks {
    async fn prepare_next_turn(
        &self,
        _ctx: &HookContext,
        _assistant: &AssistantMessage,
        _tool_results: &[ToolResultMessage],
    ) -> Result<Vec<Message>, HookError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(HookError::new("rejected"))
    }
}

/// MergedHooks prepare_next_turn 短路：a 注入、b 拒绝 → 保留 a 的注入。
#[tokio::test]
async fn test_merged_prepare_next_turn_short_circuit_keeps_success() {
    let calls_a = Arc::new(AtomicUsize::new(0));
    let calls_b = Arc::new(AtomicUsize::new(0));
    let merged = MergedHooks::new(vec![
        (
            "a".to_string(),
            Arc::new(InjectingHooksN {
                text: "from-a".to_string(),
                calls: calls_a.clone(),
            }) as Arc<dyn LifecycleHooks>,
        ),
        (
            "b".to_string(),
            Arc::new(PrepareRejectingHooks {
                calls: calls_b.clone(),
            }) as Arc<dyn LifecycleHooks>,
        ),
    ]);
    let out = merged
        .prepare_next_turn(&ctx(), &assistant(), &[tool_result_msg()])
        .await
        .expect("should be Ok (short-circuit keeps success)");
    assert_eq!(out.len(), 1, "should keep a's injection");
    assert_eq!(calls_a.load(Ordering::SeqCst), 1);
    assert_eq!(calls_b.load(Ordering::SeqCst), 1);
}
