//! 上下文预算与裁剪，以及 `convert_to_llm` 的通用投影。
//!
//! 一期：每轮请求前按模型 `context_window` 粗估 token；超限做**保守截断**
//! （从最旧消息丢弃，保留最近消息）。二期在此接入摘要压缩（`Compactor` 预留）。
//!
//! token 估算为粗估（字节数 / 4 + 1），不追求精确，只用于预算判断。

use std::sync::Arc;

use crate::core::message::{AssistantContent, Message, ToolResultContent, UserContent};

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
    /// 已预算内则原样返回。若单条消息即超窗，仍保留最近一条（不丢空）。
    pub fn truncate(&self, messages: Vec<Arc<Message>>) -> Vec<Arc<Message>> {
        if self.fits(&messages) {
            return messages;
        }
        let len = messages.len();
        let mut start = 0;
        while start + 1 < len && !self.fits(&messages[start..]) {
            start += 1;
        }
        messages[start..].to_vec()
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

    #[test]
    fn test_budget_fits_and_truncate() {
        // 每条消息 400 字节 ≈ 101 token。窗口 250 → 最多放 2 条。
        let budget = ContextBudget::new(250);
        let msgs: Vec<Arc<Message>> = (0..5)
            .map(|i| user_msg(&format!("m{i}{}", "x".repeat(400))))
            .collect();
        assert!(!budget.fits(&msgs), "5 条应超预算");
        let truncated = budget.truncate(msgs.clone());
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
        let truncated = budget.truncate(msgs);
        assert_eq!(truncated.len(), 1, "超窗单条也应保留最近一条");
    }

    #[test]
    fn test_default_convert_to_llm() {
        let msgs = vec![user_msg("hi")];
        let converted = default_convert_to_llm(msgs);
        assert_eq!(converted.len(), 1);
        assert!(matches!(converted[0], Message::User(_)));
    }
}
