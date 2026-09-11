//! [`super`] 单测：默认实现 + 单 hook 行为（改写 / 注入）。
//!
//! 自 `hooks.rs` 拆出（单文件 ≤ 400 行约束，Task 029）。MergedHooks 合并语义
//! 测试见 [`super::tests_merged`] / [`super::tests_merged_stop`]。

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::core::message::{AssistantContent, StopReason, ToolResultContent};

/// 构造一个空 transcript 的 HookContext。
pub fn ctx() -> HookContext {
    HookContext::new(&[])
}

/// 构造一个 ToolCall。
pub fn tool_call() -> ToolCall {
    ToolCall {
        id: "c1".to_string(),
        name: "echo".to_string(),
        arguments: "{}".to_string(),
    }
}

/// 构造一个 ToolResult。
pub fn tool_result(text: &str) -> ToolResult {
    ToolResult {
        content: vec![ToolResultContent::Text {
            text: text.to_string(),
        }],
        is_error: false,
        details: None,
    }
}

/// 构造一个 AssistantMessage。
pub fn assistant() -> AssistantMessage {
    AssistantMessage {
        content: vec![AssistantContent::Text {
            text: "hi".to_string(),
        }],
        model: None,
        usage: None,
        stop_reason: Some(StopReason::Completed),
        error_message: None,
        timestamp: 0,
    }
}

/// 构造一个 ToolResultMessage。
pub fn tool_result_msg() -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: "c1".to_string(),
        tool_name: "echo".to_string(),
        is_error: false,
        content: vec![ToolResultContent::Text {
            text: "ok".to_string(),
        }],
        details: None,
        timestamp: 0,
    }
}

/// 默认实现：未覆盖的方法不 panic、不干预主循环（空操作）。
pub struct NoopHooks;

#[async_trait]
impl LifecycleHooks for NoopHooks {}

/// 默认 before_tool_call 返回 Ok(())。
#[tokio::test]
async fn test_default_before_tool_call_ok() {
    let hooks = NoopHooks;
    let result = hooks.before_tool_call(&ctx(), &serde_json::json!({})).await;
    assert!(result.is_ok(), "default before_tool_call should be Ok");
}

/// 默认 after_tool_call 原样透传（不改写）。
#[tokio::test]
async fn test_default_after_tool_call_passthrough() {
    let hooks = NoopHooks;
    let result = tool_result("original");
    let out = hooks
        .after_tool_call(&ctx(), &tool_call(), result.clone())
        .await
        .expect("default after_tool_call should be Ok");
    assert_eq!(out, result, "default after_tool_call should pass through");
}

/// 默认 should_stop_after_turn 返回 None。
#[test]
fn test_default_should_stop_none() {
    let hooks = NoopHooks;
    assert_eq!(hooks.should_stop_after_turn(&ctx()), None);
}

/// 默认 prepare_next_turn 返回空 Vec（不注入）。
#[tokio::test]
async fn test_default_prepare_next_turn_empty() {
    let hooks = NoopHooks;
    let out = hooks
        .prepare_next_turn(&ctx(), &assistant(), &[tool_result_msg()])
        .await
        .expect("default prepare_next_turn should be Ok");
    assert!(out.is_empty(), "default prepare_next_turn should be empty");
}

/// 改写型 hook：after_tool_call 返回改写后的 ToolResult。
struct RewritingHooks {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LifecycleHooks for RewritingHooks {
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
                text: format!("rewritten:{text}"),
            }],
            is_error: result.is_error,
            details: None,
        })
    }
}

impl RewritingHooks {
    fn text_of(r: &ToolResult) -> String {
        match &r.content[0] {
            ToolResultContent::Text { text } => text.clone(),
            _ => unreachable!(),
        }
    }
}

/// after_tool_call 改写：fake hook 返回改写后的 ToolResult，断言采用改写值。
#[tokio::test]
async fn test_after_tool_call_rewrite() {
    let hooks = RewritingHooks {
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let result = tool_result("original");
    let out = hooks
        .after_tool_call(&ctx(), &tool_call(), result)
        .await
        .expect("should be Ok");
    assert_eq!(
        RewritingHooks::text_of(&out),
        "rewritten:original",
        "should adopt the rewritten value"
    );
}

/// 注入型 hook：prepare_next_turn 返回 Vec<Message>。
struct InjectingHooks {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LifecycleHooks for InjectingHooks {
    async fn prepare_next_turn(
        &self,
        _ctx: &HookContext,
        _assistant: &AssistantMessage,
        _tool_results: &[ToolResultMessage],
    ) -> Result<Vec<Message>, HookError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![Message::User(crate::core::message::UserMessage {
            content: vec![crate::core::message::UserContent::Text {
                text: "injected".to_string(),
            }],
            timestamp: 0,
        })])
    }
}

/// prepare_next_turn 注入：fake hook 返回 Vec<Message>，断言注入这些消息。
#[tokio::test]
async fn test_prepare_next_turn_inject() {
    let hooks = InjectingHooks {
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let out = hooks
        .prepare_next_turn(&ctx(), &assistant(), &[tool_result_msg()])
        .await
        .expect("should be Ok");
    assert_eq!(out.len(), 1, "should inject one message");
    assert!(
        matches!(out[0], Message::User(_)),
        "injected message should be User"
    );
}
