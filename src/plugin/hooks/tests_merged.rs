//! [`super::MergedHooks`] 单测：before/after_tool_call 合并语义（短路 / 按值串接 /
//! id 字典序）。
//!
//! 自 `hooks.rs` 拆出（单文件 ≤ 400 行约束，Task 029）。共享测试 helper 在
//! [`super::tests`]。

use super::tests::{ctx, tool_call, tool_result};
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::core::message::ToolResultContent;

/// MergedHooks：before_tool_call 任一 Err 短路（后续插件不被调用，用调用计数断言）。
struct BeforeRejectingHooks {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LifecycleHooks for BeforeRejectingHooks {
    async fn before_tool_call(
        &self,
        _ctx: &HookContext,
        _args: &serde_json::Value,
    ) -> Result<(), HookError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(HookError::new("rejected"))
    }
}

struct BeforePassingHooks {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LifecycleHooks for BeforePassingHooks {
    async fn before_tool_call(
        &self,
        _ctx: &HookContext,
        _args: &serde_json::Value,
    ) -> Result<(), HookError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// MergedHooks before_tool_call 短路：a 通过、b 拒绝 → b 被调用、c 不被调用。
#[tokio::test]
async fn test_merged_before_tool_call_short_circuit() {
    let calls_a = Arc::new(AtomicUsize::new(0));
    let calls_b = Arc::new(AtomicUsize::new(0));
    let calls_c = Arc::new(AtomicUsize::new(0));
    let merged = MergedHooks::new(vec![
        (
            "a".to_string(),
            Arc::new(BeforePassingHooks {
                calls: calls_a.clone(),
            }) as Arc<dyn LifecycleHooks>,
        ),
        (
            "b".to_string(),
            Arc::new(BeforeRejectingHooks {
                calls: calls_b.clone(),
            }) as Arc<dyn LifecycleHooks>,
        ),
        (
            "c".to_string(),
            Arc::new(BeforePassingHooks {
                calls: calls_c.clone(),
            }) as Arc<dyn LifecycleHooks>,
        ),
    ]);
    let result = merged
        .before_tool_call(&ctx(), &serde_json::json!({}))
        .await;
    assert!(result.is_err(), "should be Err (b rejected)");
    assert_eq!(calls_a.load(Ordering::SeqCst), 1, "a should be called");
    assert_eq!(calls_b.load(Ordering::SeqCst), 1, "b should be called");
    assert_eq!(
        calls_c.load(Ordering::SeqCst),
        0,
        "c should NOT be called (short-circuited)"
    );
}

/// MergedHooks after_tool_call 按值串接：a 改写 → b 再改写，断言最终值。
struct AfterRewritingHooks {
    prefix: String,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LifecycleHooks for AfterRewritingHooks {
    async fn after_tool_call(
        &self,
        _ctx: &HookContext,
        _tool_call: &ToolCall,
        result: ToolResult,
    ) -> Result<ToolResult, HookError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let text = match &result.content[0] {
            ToolResultContent::Text { text } => text.clone(),
            _ => String::new(),
        };
        Ok(ToolResult {
            content: vec![ToolResultContent::Text {
                text: format!("{}:{}", self.prefix, text),
            }],
            is_error: result.is_error,
            details: None,
        })
    }
}

/// MergedHooks after_tool_call 按值串接：a 前缀 → b 前缀，断言 b 收到 a 的改写值。
#[tokio::test]
async fn test_merged_after_tool_call_chained() {
    let calls_a = Arc::new(AtomicUsize::new(0));
    let calls_b = Arc::new(AtomicUsize::new(0));
    let merged = MergedHooks::new(vec![
        (
            "a".to_string(),
            Arc::new(AfterRewritingHooks {
                prefix: "a".to_string(),
                calls: calls_a.clone(),
            }) as Arc<dyn LifecycleHooks>,
        ),
        (
            "b".to_string(),
            Arc::new(AfterRewritingHooks {
                prefix: "b".to_string(),
                calls: calls_b.clone(),
            }) as Arc<dyn LifecycleHooks>,
        ),
    ]);
    let result = tool_result("orig");
    let out = merged
        .after_tool_call(&ctx(), &tool_call(), result)
        .await
        .expect("should be Ok");
    let text = match &out.content[0] {
        ToolResultContent::Text { text } => text.clone(),
        _ => unreachable!(),
    };
    assert_eq!(
        text, "b:a:orig",
        "b should receive a's rewritten value and prepend its own prefix"
    );
    assert_eq!(calls_a.load(Ordering::SeqCst), 1);
    assert_eq!(calls_b.load(Ordering::SeqCst), 1);
}

/// MergedHooks after_tool_call 短路：a 通过、b 拒绝 → 采用 a 的改写值（最后一个成功值）。
struct AfterRejectingHooks {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LifecycleHooks for AfterRejectingHooks {
    async fn after_tool_call(
        &self,
        _ctx: &HookContext,
        _tool_call: &ToolCall,
        _result: ToolResult,
    ) -> Result<ToolResult, HookError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(HookError::new("rejected"))
    }
}

/// MergedHooks after_tool_call 短路：a 改写、b 拒绝 → 采用 a 的改写值。
#[tokio::test]
async fn test_merged_after_tool_call_short_circuit_keeps_last_success() {
    let calls_a = Arc::new(AtomicUsize::new(0));
    let calls_b = Arc::new(AtomicUsize::new(0));
    let merged = MergedHooks::new(vec![
        (
            "a".to_string(),
            Arc::new(AfterRewritingHooks {
                prefix: "a".to_string(),
                calls: calls_a.clone(),
            }) as Arc<dyn LifecycleHooks>,
        ),
        (
            "b".to_string(),
            Arc::new(AfterRejectingHooks {
                calls: calls_b.clone(),
            }) as Arc<dyn LifecycleHooks>,
        ),
    ]);
    let result = tool_result("orig");
    let out = merged
        .after_tool_call(&ctx(), &tool_call(), result)
        .await
        .expect("should be Ok (short-circuit keeps last success)");
    let text = match &out.content[0] {
        ToolResultContent::Text { text } => text.clone(),
        _ => unreachable!(),
    };
    assert_eq!(
        text, "a:orig",
        "should keep a's rewritten value (last success before b's Err)"
    );
    assert_eq!(calls_a.load(Ordering::SeqCst), 1);
    assert_eq!(calls_b.load(Ordering::SeqCst), 1);
}

/// MergedHooks 按 id 字典序排序：传入乱序 → 内部按 id 排序。
#[tokio::test]
async fn test_merged_hooks_sorted_by_id() {
    let calls_z = Arc::new(AtomicUsize::new(0));
    let calls_a = Arc::new(AtomicUsize::new(0));
    // 故意乱序传入：z 在前、a 在后。
    let merged = MergedHooks::new(vec![
        (
            "z".to_string(),
            Arc::new(BeforeRejectingHooks {
                calls: calls_z.clone(),
            }) as Arc<dyn LifecycleHooks>,
        ),
        (
            "a".to_string(),
            Arc::new(BeforeRejectingHooks {
                calls: calls_a.clone(),
            }) as Arc<dyn LifecycleHooks>,
        ),
    ]);
    // a 先被调用（字典序），a 拒绝 → 短路，z 不被调用。
    let result = merged
        .before_tool_call(&ctx(), &serde_json::json!({}))
        .await;
    assert!(result.is_err());
    assert_eq!(
        calls_a.load(Ordering::SeqCst),
        1,
        "a (first in dict order) should be called"
    );
    assert_eq!(
        calls_z.load(Ordering::SeqCst),
        0,
        "z should NOT be called (short-circuited by a)"
    );
}
