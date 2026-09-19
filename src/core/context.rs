//! 上下文预算与裁剪，以及 `convert_to_llm` 的通用投影。
//!
//! 一期：每轮请求前按模型 `context_window` 粗估 token；超限做**保守截断**
//! （从最旧消息丢弃，保留最近消息）。
//! 二期：`plan_context` 编排——超预算时把较早消息压缩为一条摘要（保留最近
//! `keep_recent` 个完整 turn），摘要注入 transcript；压缩失败降级为保守截断，
//! 不阻断运行。
//!
//! **提交纪律（Task 041）**：`plan_context` 只产出「请求投影 + 可选提交计划」，
//! 不直接改写 transcript。只有压缩成功（`commit: Some`）才发生 transcript/session
//! 变更；取消（`Err(Cancelled)`）与普通失败均不丢历史。
//!
//! token 估算为粗估（字节数 / 4 + 1），不追求精确，只用于预算判断。

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::core::compactor::{CompactionError, CompactionRequest, Compactor};
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

/// 粗估消息列表的总 token 数（u64 累加，避免 u32 求和溢出）。
fn estimate_total(messages: &[Arc<Message>]) -> u64 {
    messages
        .iter()
        .map(|m| estimate_message_tokens(m) as u64)
        .sum()
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
    /// 委托 `truncate_to_budget`（拓扑安全，turn/user boundary 粒度）。
    pub fn truncate(&self, messages: &[Arc<Message>]) -> Vec<Arc<Message>> {
        truncate_to_budget(messages, self.context_window as usize)
    }
}

/// 压缩策略：触发压缩的 token 阈值 + 保留最近完整 turn 数。
///
/// 默认保守（`budget_tokens` 极大、`keep_recent` 小），等价于「几乎不压缩」，
/// 与一期行为兼容。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionPolicy {
    /// 触发压缩的 token 阈值（粗估）。
    pub budget_tokens: usize,
    /// 保留最近完整 turn 数（不压缩）。
    ///
    /// Task 041：升级为 turn 粒度——保留最近 N 个完整 turn（非 N 条消息）。
    /// 一个 turn = 一条 `User` 消息 + 其后的所有 `Assistant`/`ToolResult` 消息，
    /// 直到下一条 `User`（不含）。
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

/// 上下文准备结果：本轮请求投影 + 可选提交计划。
///
/// - `request_messages`：本轮发送给 provider 的消息（投影）。
/// - `commit`：仅在压缩成功时 `Some`——提交计划（用 `[User(summary)]` 替换
///   `transcript[0..keep_from]`）。`None` 表示不改写 transcript。
#[derive(Debug, Clone)]
pub struct PreparedContext {
    /// 本轮发送给 provider 的消息（投影）。
    pub request_messages: Vec<Arc<Message>>,
    /// 仅在压缩成功时 `Some`：提交计划。
    pub commit: Option<CompactionCommit>,
}

/// 压缩提交计划：摘要文本 + 保留起点。
///
/// 提交 = 用 `[User(summary)]` 替换 `transcript[0..keep_from]`，并持久化到
/// session（JSONL）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionCommit {
    /// 摘要文本。
    pub summary: String,
    /// 提交 = 用 `[User(summary)]` 替换 `transcript[0..keep_from]`。
    pub keep_from: usize,
}

/// 上下文准备（二期编排）：预算检查 → 必要时压缩 → 产出请求投影 + 可选提交计划。
///
/// - 未超预算：原样返回（不压缩），`commit: None`。
/// - 超预算 + 压缩成功：`request_messages = [User(summary)] ++ keep`，
///   `commit: Some(CompactionCommit { summary, keep_from })`。
/// - 超预算 + 取消：返回 `Err(CompactionError::Cancelled)`（向上传播，终止 run，
///   transcript 原样）。
/// - 超预算 + 其他失败（Provider/EmptyInput/EmptySummary）：
///   `request_messages = truncate_to_budget(transcript)`，`commit: None`
///   （仅本次请求临时截断，不改写 transcript）。
///
/// **不变式**：只有压缩成功（`commit: Some`）才发生 transcript/session 变更；
/// 取消与普通失败均不丢历史。
pub async fn plan_context(
    transcript: &[Arc<Message>],
    policy: &CompactionPolicy,
    compactor: &dyn Compactor,
    signal: CancellationToken,
) -> Result<PreparedContext, CompactionError> {
    // 粗估总 token（u64 累加，避免 u32 求和溢出与 usize→u32 回绕）。
    if estimate_total(transcript) <= policy.budget_tokens as u64 {
        return Ok(PreparedContext {
            request_messages: transcript.to_vec(),
            commit: None,
        });
    }
    // 超预算：按 turn 边界分界，前 (num_turns - keep_recent) 个 turn 待压缩。
    let split = compute_split(transcript, policy.keep_recent);
    if split == 0 {
        // 消息本就很少（不足 keep_recent+1 个 turn），不压缩，降级为临时截断。
        return Ok(PreparedContext {
            request_messages: truncate_to_budget(transcript, policy.budget_tokens),
            commit: None,
        });
    }
    let (to_compact, keep) = transcript.split_at(split);
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
                    text: result.summary.clone(),
                }],
                timestamp: 0,
            }));
            let mut out = Vec::with_capacity(keep.len() + 1);
            out.push(summary_msg);
            out.extend_from_slice(keep);
            Ok(PreparedContext {
                request_messages: out,
                commit: Some(CompactionCommit {
                    summary: result.summary,
                    keep_from: split,
                }),
            })
        }
        Err(CompactionError::Cancelled) => Err(CompactionError::Cancelled),
        Err(_other) => {
            // 降级：保守截断（仅本次请求临时截断，不改写 transcript）。
            Ok(PreparedContext {
                request_messages: truncate_to_budget(transcript, policy.budget_tokens),
                commit: None,
            })
        }
    }
}

/// 拓扑安全截断：按 turn/user boundary 丢弃整 turn，保证 tool call/result 成组。
///
/// 1. `estimate_total(messages) <= max_tokens` → 原样返回。
/// 2. 否则从头部整 turn 丢弃（切点推进到下一条 `User` 边界），直到满足预算。
/// 3. 若丢弃到只剩最后一个 turn 仍超预算 → 至少保留最后 1 个 turn，不产出空列表。
/// 4. 防御：若 `messages[0]` 非 `User`（不应发生），仍以首条为切点，
///    但不得切断 tool call/result 成组（即切点前一条不得是含 `ToolCall` 的
///    `Assistant` 且切点后是 `ToolResult`）。
pub fn truncate_to_budget(messages: &[Arc<Message>], max_tokens: usize) -> Vec<Arc<Message>> {
    if estimate_total(messages) <= max_tokens as u64 {
        return messages.to_vec();
    }
    let len = messages.len();
    if len == 0 {
        return Vec::new();
    }
    // 找所有 turn 边界（`User` 消息的索引）。
    let mut boundaries: Vec<usize> = (0..len)
        .filter(|&i| matches!(messages[i].as_ref(), Message::User(_)))
        .collect();
    // 防御：若首条非 `User`，以首条为切点。
    if !matches!(messages[0].as_ref(), Message::User(_)) {
        boundaries.insert(0, 0);
    }
    if boundaries.is_empty() {
        // 无 `User` 消息（不应发生）：保留最后一条。
        return messages[len - 1..].to_vec();
    }
    // 找最小的边界，使剩余消息满足预算。
    for &boundary in &boundaries {
        // 防御：不得切断 tool call/result 成组。
        if boundary > 0 && is_tool_call_result_split(&messages[boundary - 1], &messages[boundary]) {
            continue;
        }
        if estimate_total(&messages[boundary..]) <= max_tokens as u64 {
            return messages[boundary..].to_vec();
        }
    }
    // 即使最后一个 turn 仍超预算：至少保留最后 1 个 turn。
    let last_boundary = *boundaries.last().expect("boundaries is non-empty");
    messages[last_boundary..].to_vec()
}

/// 判断切点是否切断 tool call/result 成组（切点前一条是含 `ToolCall` 的
/// `Assistant` 且切点后是 `ToolResult`）。
fn is_tool_call_result_split(prev: &Arc<Message>, next: &Arc<Message>) -> bool {
    let prev_has_tool_call = matches!(prev.as_ref(), Message::Assistant(a)
        if a.content.iter().any(|c| matches!(c, AssistantContent::ToolCall(_))));
    let next_is_tool_result = matches!(next.as_ref(), Message::ToolResult(_));
    prev_has_tool_call && next_is_tool_result
}

/// 按 turn 边界计算压缩分界点：保留最近 `keep_recent` 个完整 turn，
/// 返回待压缩消息的条数（切点索引）。
///
/// - 无 `User` 消息（不应发生）：返回 0（不压缩）。
/// - `keep_recent >= num_turns`：返回 0（保留所有 turn，不压缩）。
/// - 否则：返回第 `(num_turns - keep_recent)` 个 turn 的起点（0-indexed）。
fn compute_split(messages: &[Arc<Message>], keep_recent: usize) -> usize {
    let len = messages.len();
    // 找所有 turn 边界（`User` 消息的索引）。
    let boundaries: Vec<usize> = (0..len)
        .filter(|&i| matches!(messages[i].as_ref(), Message::User(_)))
        .collect();
    if boundaries.is_empty() {
        return 0;
    }
    let num_turns = boundaries.len();
    // `keep_recent` 至少为 1（保留最后 1 个 turn）。
    let keep = keep_recent.max(1);
    if keep >= num_turns {
        return 0; // 保留所有 turn，不压缩。
    }
    // 切点 = 第 (num_turns - keep) 个 turn 的起点（0-indexed）。
    boundaries[num_turns - keep]
}

/// 默认 `convert_to_llm` 投影：把 `Arc<Message>` transcript 投影为 owned
/// `Vec<Message>`（供 provider 请求使用）。
pub fn default_convert_to_llm(messages: Vec<Arc<Message>>) -> Vec<Message> {
    messages.into_iter().map(|m| (*m).clone()).collect()
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_truncate;
