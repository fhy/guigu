//! `context` 模块单元测试（fake compactor 驱动，不依赖网络）。

use super::*;
use crate::core::message::{
    AssistantContent, AssistantMessage, StopReason, ToolCall, ToolResultContent, ToolResultMessage,
};

pub(crate) fn user_msg(text: &str) -> Arc<Message> {
    Arc::new(Message::User(UserMessage {
        content: vec![UserContent::Text {
            text: text.to_string(),
        }],
        timestamp: 0,
    }))
}

pub(crate) fn assistant_tool_call(id: &str, name: &str) -> Arc<Message> {
    Arc::new(Message::Assistant(AssistantMessage {
        content: vec![AssistantContent::ToolCall(ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: "{}".to_string(),
        })],
        model: None,
        usage: None,
        stop_reason: Some(StopReason::Completed),
        error_message: None,
        timestamp: 0,
    }))
}

pub(crate) fn tool_result(id: &str, name: &str, text: &str) -> Arc<Message> {
    Arc::new(Message::ToolResult(ToolResultMessage {
        tool_call_id: id.to_string(),
        tool_name: name.to_string(),
        is_error: false,
        content: vec![ToolResultContent::Text {
            text: text.to_string(),
        }],
        details: None,
        timestamp: 0,
    }))
}

/// 脚本化 fake compactor：记录被压缩的消息，返回固定摘要或失败。
pub(crate) struct FakeCompactor {
    summary: String,
    fail: bool,
    cancel: bool,
    calls: std::sync::Mutex<Vec<Vec<Arc<Message>>>>,
}

impl FakeCompactor {
    pub(crate) fn ok(summary: &str) -> Arc<Self> {
        Arc::new(FakeCompactor {
            summary: summary.to_string(),
            fail: false,
            cancel: false,
            calls: std::sync::Mutex::new(Vec::new()),
        })
    }
    pub(crate) fn failing() -> Arc<Self> {
        Arc::new(FakeCompactor {
            summary: String::new(),
            fail: true,
            cancel: false,
            calls: std::sync::Mutex::new(Vec::new()),
        })
    }
    pub(crate) fn cancelling() -> Arc<Self> {
        Arc::new(FakeCompactor {
            summary: String::new(),
            fail: false,
            cancel: true,
            calls: std::sync::Mutex::new(Vec::new()),
        })
    }
    pub(crate) fn call_count(&self) -> usize {
        self.calls.lock().expect("calls mutex").len()
    }
    pub(crate) fn last_compacted(&self) -> Vec<Arc<Message>> {
        self.calls
            .lock()
            .expect("calls mutex")
            .last()
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait::async_trait]
impl Compactor for FakeCompactor {
    async fn compact(
        &self,
        req: CompactionRequest,
    ) -> Result<crate::core::compactor::CompactionResult, crate::core::compactor::CompactionError>
    {
        self.calls.lock().expect("calls mutex").push(req.messages);
        if self.cancel {
            Err(crate::core::compactor::CompactionError::Cancelled)
        } else if self.fail {
            Err(crate::core::compactor::CompactionError::Provider(
                crate::core::provider::ProviderError::Request(
                    "simulated compaction failure".to_string(),
                ),
            ))
        } else {
            Ok(crate::core::compactor::CompactionResult {
                summary: self.summary.clone(),
            })
        }
    }
}

// ---------- estimate_tokens ----------

#[test]
fn test_estimate_tokens_positive() {
    assert!(estimate_tokens("") >= 1);
    assert!(estimate_tokens("a".repeat(400).as_str()) > 100);
}

/// token 估算边界：空 / 短 / 长文本的精确值（字节数 / 4 + 1）。
#[test]
fn test_estimate_tokens_boundaries() {
    assert_eq!(estimate_tokens(""), 1, "空文本应为 1（0/4+1）");
    assert_eq!(estimate_tokens("abcd"), 2, "4 字节应为 2（4/4+1）");
    assert_eq!(
        estimate_tokens("a".repeat(400).as_str()),
        101,
        "400 字节应为 101"
    );
}

// ---------- ContextBudget ----------

#[test]
fn test_budget_fits_and_truncate() {
    // 每条消息 400 字节 ≈ 101 token。窗口 250 → 最多放 2 条。
    let budget = ContextBudget::new(250);
    let msgs: Vec<Arc<Message>> = (0..5)
        .map(|i| user_msg(&format!("m{i}{}", "x".repeat(400))))
        .collect();
    assert!(!budget.fits(&msgs), "5 条应超预算");
    let truncated = budget.truncate(&msgs);
    assert!(budget.fits(&truncated), "截断后应在预算内");
    assert!(truncated.len() <= 2, "应只保留最近的消息");
    // 最近一条必须保留
    assert_eq!(
        truncated.last().unwrap().as_ref(),
        msgs.last().unwrap().as_ref()
    );
}

#[test]
fn test_budget_truncate_keeps_last_when_all_oversized() {
    // 单条消息就超窗口：截断后仍保留最近一条（不丢空）。
    let budget = ContextBudget::new(10);
    let big = user_msg(&"x".repeat(1000));
    let msgs = vec![big.clone(), big.clone()];
    let truncated = budget.truncate(&msgs);
    assert_eq!(truncated.len(), 1, "超窗单条也应保留最近一条");
}

#[test]
fn test_budget_includes_reserve_and_fixed_overhead() {
    let budget = ContextBudget::with_overhead(100, "1234", "1234", 10, 20);
    assert_eq!(budget.available(), 90);
    assert!(!budget.fits(&[user_msg(&"x".repeat(280))]));
}

#[test]
fn test_usage_baseline_is_preferred_for_compaction_budget() {
    let assistant = Arc::new(Message::Assistant(AssistantMessage {
        content: vec![AssistantContent::Text {
            text: "tiny".into(),
        }],
        model: None,
        usage: Some(crate::core::message::Usage {
            input: 100,
            output: 1,
            cache_read: 0,
            cache_write: 0,
            total_tokens: 101,
            cost: 0.0,
        }),
        stop_reason: Some(StopReason::Completed),
        error_message: None,
        timestamp: 0,
    }));
    let transcript = vec![assistant, user_msg(&"x".repeat(40))];
    let policy = CompactionPolicy {
        budget_tokens: 110,
        keep_recent: 1,
        reserve_output_tokens: 0,
        protocol_wrapper_tokens: 0,
    };
    assert!(estimate_total(&transcript, 0) > policy.budget_tokens as u64);
    let budget = ContextBudget::new(110);
    assert_eq!(
        budget.estimate(&transcript),
        estimate_total(&transcript, 0) as u32
    );
    assert!(!budget.fits(&transcript));
}

#[test]
fn test_usage_baseline_does_not_double_deduct_overhead() {
    let assistant = Arc::new(Message::Assistant(AssistantMessage {
        content: vec![AssistantContent::Text {
            text: "tiny".into(),
        }],
        model: None,
        usage: Some(crate::core::message::Usage {
            input: 90,
            output: 1,
            cache_read: 0,
            cache_write: 0,
            total_tokens: 91,
            cost: 0.0,
        }),
        stop_reason: Some(StopReason::Completed),
        error_message: None,
        timestamp: 0,
    }));
    let budget = ContextBudget::with_overhead(100, "x".repeat(40).as_str(), "", 0, 0);
    let transcript = vec![assistant];
    assert_eq!(budget.available(), 100);
    assert!(budget.fits(&transcript));
}

#[test]
fn test_fallback_estimate_includes_fixed_overhead() {
    let budget = ContextBudget::with_overhead(100, "x".repeat(40).as_str(), "", 0, 0);
    let transcript = vec![user_msg("x".repeat(400).as_str())];
    assert_eq!(budget.available(), 100);
    assert!(!budget.fits(&transcript));
    assert!(budget.estimate(&[]) >= budget.fixed_overhead);
}

// ---------- default_convert_to_llm ----------

#[test]
fn test_default_convert_to_llm() {
    let msgs = vec![user_msg("hi")];
    let converted = default_convert_to_llm(msgs);
    assert_eq!(converted.len(), 1);
    assert!(matches!(converted[0], Message::User(_)));
}

// ---------- plan_context ----------

/// 未超预算：原样返回，不触发压缩，commit: None。
#[tokio::test]
async fn test_plan_context_within_budget_no_compaction() {
    let compactor = FakeCompactor::ok("SUMMARY");
    let policy = CompactionPolicy {
        budget_tokens: 10_000,
        keep_recent: 1,
        reserve_output_tokens: 0,
        protocol_wrapper_tokens: 0,
    };
    let msgs = vec![user_msg("a"), user_msg("b")];
    let out = plan_context(&msgs, &policy, compactor.as_ref(), CancellationToken::new())
        .await
        .expect("should succeed");
    assert_eq!(out.request_messages, msgs, "未超预算应原样返回");
    assert!(out.commit.is_none(), "未超预算不应有 commit");
    assert_eq!(compactor.call_count(), 0, "未超预算不应调用 compactor");
}

/// 超预算：旧消息被摘要替换，最近 keep_recent 个 turn 保留，commit: Some。
#[tokio::test]
async fn test_plan_context_over_budget_compacts() {
    let compactor = FakeCompactor::ok("SUMMARY");
    let policy = CompactionPolicy {
        budget_tokens: 200,
        keep_recent: 1,
        reserve_output_tokens: 0,
        protocol_wrapper_tokens: 0,
    };
    // 每条 400 字节 ≈ 101 token，3 条 ≈ 303 > 200。
    let m0 = user_msg(&format!("m0{}", "x".repeat(400)));
    let m1 = user_msg(&format!("m1{}", "x".repeat(400)));
    let m2 = user_msg(&format!("m2{}", "x".repeat(400)));
    let out = plan_context(
        &[m0.clone(), m1.clone(), m2.clone()],
        &policy,
        compactor.as_ref(),
        CancellationToken::new(),
    )
    .await
    .expect("should succeed");
    assert_eq!(out.request_messages.len(), 2, "摘要 + 保留 1 条");
    // 第一条是摘要 User 消息。
    assert!(
        matches!(out.request_messages[0].as_ref(), Message::User(u) if u.content.first()
            == Some(&UserContent::Text { text: "SUMMARY".to_string() })),
        "第一条应为摘要 User 消息"
    );
    // 第二条是保留的最近消息。
    assert_eq!(out.request_messages[1], m2, "最近一条应保留");
    // commit: Some，keep_from = 2（前 2 条被替换）。
    let commit = out.commit.expect("should have commit");
    assert_eq!(commit.summary, "SUMMARY");
    assert_eq!(commit.keep_from, 2, "keep_from 应为前 2 条");
    // compactor 收到的是前 2 条（待压缩）。
    let compacted = compactor.last_compacted();
    assert_eq!(
        compacted,
        vec![m0, m1],
        "应压缩前 (num_turns-keep_recent) 个 turn"
    );
}

/// 压缩失败（Provider 错误）：降级为临时截断，commit: None（不改写 transcript）。
#[tokio::test]
async fn test_plan_context_compaction_failure_degrades() {
    let compactor = FakeCompactor::failing();
    let policy = CompactionPolicy {
        budget_tokens: 200,
        keep_recent: 1,
        reserve_output_tokens: 0,
        protocol_wrapper_tokens: 0,
    };
    let m0 = user_msg(&format!("m0{}", "x".repeat(400)));
    let m1 = user_msg(&format!("m1{}", "x".repeat(400)));
    let m2 = user_msg(&format!("m2{}", "x".repeat(400)));
    let out = plan_context(
        &[m0, m1, m2.clone()],
        &policy,
        compactor.as_ref(),
        CancellationToken::new(),
    )
    .await
    .expect("should succeed (degrade, not error)");
    // 降级为临时截断：仅保留满足预算的最近消息。
    assert!(out.request_messages.len() < 3, "应截断");
    assert!(
        out.commit.is_none(),
        "失败不应有 commit（不改写 transcript）"
    );
    assert_eq!(compactor.call_count(), 1, "应尝试压缩一次");
}

/// 压缩取消：返回 Err(Cancelled)，transcript 原样。
#[tokio::test]
async fn test_plan_context_cancelled_returns_error() {
    let compactor = FakeCompactor::cancelling();
    let policy = CompactionPolicy {
        budget_tokens: 200,
        keep_recent: 1,
        reserve_output_tokens: 0,
        protocol_wrapper_tokens: 0,
    };
    let m0 = user_msg(&format!("m0{}", "x".repeat(400)));
    let m1 = user_msg(&format!("m1{}", "x".repeat(400)));
    let m2 = user_msg(&format!("m2{}", "x".repeat(400)));
    let result = plan_context(
        &[m0, m1, m2],
        &policy,
        compactor.as_ref(),
        CancellationToken::new(),
    )
    .await;
    assert!(
        matches!(result, Err(CompactionError::Cancelled)),
        "取消应返回 Err(Cancelled)"
    );
    assert_eq!(compactor.call_count(), 1, "应尝试压缩一次");
}

/// 消息不足 keep_recent+1 个 turn（split == 0）：不压缩，降级为临时截断。
#[tokio::test]
async fn test_plan_context_too_few_turns_no_compaction() {
    let compactor = FakeCompactor::ok("SUMMARY");
    let policy = CompactionPolicy {
        budget_tokens: 1,
        keep_recent: 2,
        reserve_output_tokens: 0,
        protocol_wrapper_tokens: 0,
    };
    let big = user_msg(&"x".repeat(400));
    let out = plan_context(
        std::slice::from_ref(&big),
        &policy,
        compactor.as_ref(),
        CancellationToken::new(),
    )
    .await
    .expect("should succeed");
    // split == 0：不压缩，降级为临时截断（保留最后 1 个 turn）。
    assert_eq!(
        out.request_messages,
        vec![big],
        "split==0 应保留最后 1 个 turn"
    );
    assert!(out.commit.is_none(), "split==0 不应有 commit");
    assert_eq!(compactor.call_count(), 0, "split==0 不应调用 compactor");
}

/// 持久化语义：压缩结果回写 transcript 后，后续调用（预算内）不重复压缩、
/// 不恢复旧消息（验证「非单次请求投影」的持久契约）。
#[tokio::test]
async fn test_plan_context_persistent_no_recompact() {
    let compactor = FakeCompactor::ok("SUMMARY");
    let policy = CompactionPolicy {
        budget_tokens: 200,
        keep_recent: 1,
        reserve_output_tokens: 0,
        protocol_wrapper_tokens: 0,
    };
    // 第一次：3 条大消息（超预算）→ 压缩为 [摘要, m2]。
    let m0 = user_msg(&format!("m0{}", "x".repeat(400)));
    let m1 = user_msg(&format!("m1{}", "x".repeat(400)));
    let m2 = user_msg(&format!("m2{}", "x".repeat(400)));
    let first = plan_context(
        &[m0.clone(), m1.clone(), m2.clone()],
        &policy,
        compactor.as_ref(),
        CancellationToken::new(),
    )
    .await
    .expect("should succeed");
    assert_eq!(first.request_messages.len(), 2, "摘要 + 保留 1 条");
    assert!(first.commit.is_some(), "第一次应有 commit");
    assert_eq!(compactor.call_count(), 1, "第一次应压缩");

    // 第二次：以第一次结果（已回写 transcript，预算内）为输入 → 不重复压缩。
    let second = plan_context(
        &first.request_messages,
        &policy,
        compactor.as_ref(),
        CancellationToken::new(),
    )
    .await
    .expect("should succeed");
    assert_eq!(
        second.request_messages, first.request_messages,
        "预算内应原样返回（不重复压缩）"
    );
    assert!(second.commit.is_none(), "第二次不应有 commit");
    assert_eq!(compactor.call_count(), 1, "第二次不应再调用 compactor");
    // 旧消息 m0/m1 不恢复。
    assert!(!second.request_messages.contains(&m0), "m0 不应恢复");
    assert!(!second.request_messages.contains(&m1), "m1 不应恢复");
}
