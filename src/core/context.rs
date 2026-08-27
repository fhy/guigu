//! 上下文预算与裁剪，以及 `convert_to_llm` 的通用投影。
//!
//! 一期：每轮请求前按模型 `context_window` 粗估 token；超限做**保守截断**
//! （从最旧消息丢弃，保留最近消息）。
//! 二期：`prepare_context` 编排——超预算时把较早消息压缩为一条摘要（保留最近
//! `keep_recent` 条），摘要注入 transcript；压缩失败降级为保守截断，不阻断运行。
//!
//! token 估算为粗估（字节数 / 4 + 1），不追求精确，只用于预算判断。

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::core::compactor::{CompactionRequest, Compactor};
use crate::core::message::{
    AssistantContent, Message, ToolResultContent, UserContent, UserMessage,
};

/// 粗估一段文本的 token 数（字节数 / 4 + 1 开销）。
pub fn estimate_tokens(text: &str) -> u32 {
    text.len() as u32 / 4 + 1
}

/// 粗估单条消息的 token 数。
pub fn estimate_message_tokens(msg: &Message) -> u32 {
    match msg {
        Message::User(u) => u
            .content
            .iter()
            .map(|c| match c {
                UserContent::Text { text } => estimate_tokens(text),
                UserContent::Image(img) => estimate_tokens(&img.data) + 8,
            })
            .sum(),
        Message::Assistant(a) => a
            .content
            .iter()
            .map(|c| match c {
                AssistantContent::Text { text } => estimate_tokens(text),
                AssistantContent::Thinking { text } => estimate_tokens(text),
                AssistantContent::ToolCall(tc) => {
                    estimate_tokens(&tc.name) + estimate_tokens(&tc.arguments)
                }
            })
            .sum(),
        Message::ToolResult(t) => {
            estimate_tokens(&t.tool_name)
                + t.content
                    .iter()
                    .map(|c| match c {
                        ToolResultContent::Text { text } => estimate_tokens(text),
                        ToolResultContent::Image(img) => estimate_tokens(&img.data) + 8,
                    })
                    .sum::<u32>()
        }
    }
}

/// 上下文预算：按模型 `context_window` 判断与裁剪。
#[derive(Debug, Clone, Copy)]
pub struct ContextBudget {
    pub context_window: u32,
}

impl ContextBudget {
    pub fn new(context_window: u32) -> Self {
        ContextBudget { context_window }
    }

    /// 粗估消息列表的总 token 数。
    pub fn estimate(&self, messages: &[Arc<Message>]) -> u32 {
        messages.iter().map(|m| estimate_message_tokens(m)).sum()
    }

    /// 消息列表是否在预算内。
    pub fn fits(&self, messages: &[Arc<Message>]) -> bool {
        self.estimate(messages) <= self.context_window
    }

    /// 保守截断：从最旧（前端）丢弃直到预算内，始终保留最近一条消息。
    ///
    /// 接受借用切片，仅 clone 保留下来的 Arc 指针（已预算内则 clone 全量）。
    /// 若单条消息即超窗，仍保留最近一条（不丢空）。
    pub fn truncate(&self, messages: &[Arc<Message>]) -> Vec<Arc<Message>> {
        if self.fits(messages) {
            return messages.to_vec();
        }
        let len = messages.len();
        let mut start = 0;
        while start + 1 < len && !self.fits(&messages[start..]) {
            start += 1;
        }
        messages[start..].to_vec()
    }
}

/// 压缩策略：触发压缩的 token 阈值 + 保留最近消息条数。
///
/// 默认保守（`budget_tokens` 极大、`keep_recent` 小），等价于「几乎不压缩」，
/// 与一期行为兼容。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionPolicy {
    /// 触发压缩的 token 阈值（粗估）。
    pub budget_tokens: usize,
    /// 保留最近消息条数（不压缩）。
    pub keep_recent: usize,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        CompactionPolicy {
            budget_tokens: usize::MAX,
            keep_recent: 1,
        }
    }
}

/// 上下文准备（二期编排）：预算检查 → 必要时压缩 → 产出最终消息列表。
///
/// - 未超预算：原样返回（不压缩）。
/// - 超预算：前 `len - keep_recent` 条压缩为一条摘要，摘要作为一条 `User` 消息
///   置于保留消息之前（不新增 `Message` 变体，不破坏 002 消息拓扑）。
/// - 压缩失败（Provider 错误 / 取消 / 空输入）：降级为保守截断——仅保留最近
///   `keep_recent` 条，不阻断运行（与一期「超限截断」契约兼容）。
pub async fn prepare_context(
    messages: Vec<Arc<Message>>,
    policy: &CompactionPolicy,
    compactor: &dyn Compactor,
    signal: CancellationToken,
) -> Vec<Arc<Message>> {
    // 粗估总 token（u64 累加，避免 u32 求和溢出与 usize→u32 回绕）。
    let total: u64 = messages
        .iter()
        .map(|m| estimate_message_tokens(m) as u64)
        .sum();
    if total <= policy.budget_tokens as u64 {
        return messages;
    }
    // 分界：前 (len - keep_recent) 条待压缩，后 keep_recent 条保留。
    let split = messages.len().saturating_sub(policy.keep_recent);
    if split == 0 {
        // 消息本就很少（不足 keep_recent+1），不压缩。
        return messages;
    }
    let (to_compact, keep) = messages.split_at(split);
    match compactor
        .compact(CompactionRequest {
            messages: to_compact.to_vec(),
            signal,
        })
        .await
    {
        Ok(result) => {
            // 摘要注入为一条普通 User 消息，置于保留消息之前。
            let summary_msg = Arc::new(Message::User(UserMessage {
                content: vec![UserContent::Text {
                    text: result.summary,
                }],
                timestamp: 0,
            }));
            let mut out = Vec::with_capacity(keep.len() + 1);
            out.push(summary_msg);
            out.extend_from_slice(keep);
            out
        }
        Err(_) => {
            // 降级：保守截断（丢弃待压缩的旧消息，仅保留最近 keep_recent 条）。
            keep.to_vec()
        }
    }
}

/// 默认 `convert_to_llm` 投影：把 `Arc<Message>` transcript 投影为 owned
/// `Vec<Message>`（供 provider 请求使用）。
pub fn default_convert_to_llm(messages: Vec<Arc<Message>>) -> Vec<Message> {
    messages.into_iter().map(|m| (*m).clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::message::{UserContent, UserMessage};

    fn user_msg(text: &str) -> Arc<Message> {
        Arc::new(Message::User(UserMessage {
            content: vec![UserContent::Text {
                text: text.to_string(),
            }],
            timestamp: 0,
        }))
    }

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
    fn test_default_convert_to_llm() {
        let msgs = vec![user_msg("hi")];
        let converted = default_convert_to_llm(msgs);
        assert_eq!(converted.len(), 1);
        assert!(matches!(converted[0], Message::User(_)));
    }

    /// 脚本化 fake compactor：记录被压缩的消息，返回固定摘要或失败。
    struct FakeCompactor {
        summary: String,
        fail: bool,
        calls: std::sync::Mutex<Vec<Vec<Arc<Message>>>>,
    }

    impl FakeCompactor {
        fn ok(summary: &str) -> Arc<Self> {
            Arc::new(FakeCompactor {
                summary: summary.to_string(),
                fail: false,
                calls: std::sync::Mutex::new(Vec::new()),
            })
        }
        fn failing() -> Arc<Self> {
            Arc::new(FakeCompactor {
                summary: String::new(),
                fail: true,
                calls: std::sync::Mutex::new(Vec::new()),
            })
        }
        fn call_count(&self) -> usize {
            self.calls.lock().expect("calls mutex").len()
        }
        fn last_compacted(&self) -> Vec<Arc<Message>> {
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
            if self.fail {
                Err(crate::core::compactor::CompactionError::Cancelled)
            } else {
                Ok(crate::core::compactor::CompactionResult {
                    summary: self.summary.clone(),
                })
            }
        }
    }

    /// 未超预算：原样返回，不触发压缩。
    #[tokio::test]
    async fn test_prepare_context_within_budget_no_compaction() {
        let compactor = FakeCompactor::ok("SUMMARY");
        let policy = CompactionPolicy {
            budget_tokens: 10_000,
            keep_recent: 1,
        };
        let msgs = vec![user_msg("a"), user_msg("b")];
        let out = prepare_context(
            msgs.clone(),
            &policy,
            compactor.as_ref(),
            CancellationToken::new(),
        )
        .await;
        assert_eq!(out, msgs, "未超预算应原样返回");
        assert_eq!(compactor.call_count(), 0, "未超预算不应调用 compactor");
    }

    /// 超预算：旧消息被摘要替换，最近 keep_recent 条保留。
    #[tokio::test]
    async fn test_prepare_context_over_budget_compacts() {
        let compactor = FakeCompactor::ok("SUMMARY");
        let policy = CompactionPolicy {
            budget_tokens: 200,
            keep_recent: 1,
        };
        // 每条 400 字节 ≈ 101 token，3 条 ≈ 303 > 200。
        let m0 = user_msg(&format!("m0{}", "x".repeat(400)));
        let m1 = user_msg(&format!("m1{}", "x".repeat(400)));
        let m2 = user_msg(&format!("m2{}", "x".repeat(400)));
        let out = prepare_context(
            vec![m0.clone(), m1.clone(), m2.clone()],
            &policy,
            compactor.as_ref(),
            CancellationToken::new(),
        )
        .await;
        assert_eq!(out.len(), 2, "摘要 + 保留 1 条");
        // 第一条是摘要 User 消息。
        assert!(
            matches!(out[0].as_ref(), Message::User(u) if u.content.first()
                == Some(&UserContent::Text { text: "SUMMARY".to_string() })),
            "第一条应为摘要 User 消息"
        );
        // 第二条是保留的最近消息。
        assert_eq!(out[1], m2, "最近一条应保留");
        // compactor 收到的是前 2 条（待压缩）。
        let compacted = compactor.last_compacted();
        assert_eq!(compacted, vec![m0, m1], "应压缩前 (len-keep_recent) 条");
    }

    /// 压缩失败：降级为保守截断，仅保留最近 keep_recent 条。
    #[tokio::test]
    async fn test_prepare_context_compaction_failure_degrades() {
        let compactor = FakeCompactor::failing();
        let policy = CompactionPolicy {
            budget_tokens: 200,
            keep_recent: 1,
        };
        let m0 = user_msg(&format!("m0{}", "x".repeat(400)));
        let m1 = user_msg(&format!("m1{}", "x".repeat(400)));
        let m2 = user_msg(&format!("m2{}", "x".repeat(400)));
        let out = prepare_context(
            vec![m0, m1, m2.clone()],
            &policy,
            compactor.as_ref(),
            CancellationToken::new(),
        )
        .await;
        assert_eq!(out, vec![m2], "失败应仅保留最近 keep_recent 条");
        assert_eq!(compactor.call_count(), 1, "应尝试压缩一次");
    }

    /// 消息不足 keep_recent+1（split == 0）：不压缩，原样返回。
    #[tokio::test]
    async fn test_prepare_context_too_few_messages_no_compaction() {
        let compactor = FakeCompactor::ok("SUMMARY");
        let policy = CompactionPolicy {
            budget_tokens: 1,
            keep_recent: 2,
        };
        let big = user_msg(&"x".repeat(400));
        let out = prepare_context(
            vec![big.clone()],
            &policy,
            compactor.as_ref(),
            CancellationToken::new(),
        )
        .await;
        assert_eq!(out, vec![big], "split==0 应原样返回");
        assert_eq!(compactor.call_count(), 0, "split==0 不应调用 compactor");
    }
}
