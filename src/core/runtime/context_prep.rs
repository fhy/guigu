//! 每轮请求前的上下文准备（Task 041 提交纪律 + 一期最终投影）。
//!
//! 从 `mod.rs` 拆出（Task 041 r1 修复）：主循环模块保持 400 行体量限制内。
//! 职责：预算检查 + 摘要压缩编排（`plan_context`）+ 提交纪律 + 一期最终投影
//! （`transform_context` 钩子或 `context_window` 硬上限），单点实现杜绝
//! compactor 分支绕过回归。

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::core::compactor::CompactionError;
use crate::core::context::{CompactionCommit, ContextBudget, plan_context};
use crate::core::event::AgentEvent;
use crate::core::message::{AssistantMessage, Message, StopReason, UserContent, UserMessage};

use super::{RunContext, update_snapshot};

/// 每轮请求前的上下文准备结果（Task 041 提交纪律）。
pub(super) enum PreparedRequest {
    /// 取消：终止 run，产出 Aborted 终态，transcript 不变、不写 session。
    Aborted,
    /// 继续：本轮请求消息（已施加一期最终投影）。
    Messages(Vec<Arc<Message>>),
}

/// 一期最终投影（单点）：`transform_context` 钩子（最终权威）或 `context_window`
/// 硬上限截断。compactor 分支与默认分支统一经过，杜绝绕过回归（Task 041）。
///
/// `budget_tokens`（压缩触发软阈值）与 `context_window`（每轮硬上限）语义正交：
/// 本投影每轮恒定生效，不因 compactor 开启而绕过。
fn apply_final_projection(
    ctx: &RunContext,
    signal: &CancellationToken,
    request_messages: &[Arc<Message>],
) -> Vec<Arc<Message>> {
    if let Some(hook) = &ctx.config.transform_context {
        hook(request_messages.to_vec(), signal.clone())
    } else {
        let tool_schemas = ctx
            .tools
            .iter()
            .map(|tool| {
                format!(
                    "{}{}{:?}",
                    tool.name(),
                    tool.description(),
                    tool.parameters()
                )
            })
            .collect::<String>();
        let budget = ContextBudget::with_overhead(
            ctx.config.model.context_window,
            ctx.system_prompt,
            &tool_schemas,
            ctx.config.compaction.reserve_output_tokens,
            ctx.config.compaction.protocol_wrapper_tokens,
        );
        budget.truncate(request_messages)
    }
}

/// 提交压缩计划（Task 041 提交纪律）：用 `[User(summary)]` 替换
/// `transcript[0..keep_from]`，发 `MessageEnd` 持久化到 session（append-only，
/// 不物理删除旧节点），更新 snapshot。仅压缩成功（`commit: Some`）时调用。
fn commit_compaction(ctx: &mut RunContext<'_>, commit: CompactionCommit) {
    let summary_msg = Arc::new(Message::User(UserMessage {
        content: vec![UserContent::Text {
            text: commit.summary,
        }],
        timestamp: 0,
    }));
    ctx.transcript.drain(0..commit.keep_from);
    ctx.transcript.insert(0, summary_msg.clone());
    let _ = ctx.events_tx.send(AgentEvent::MessageEnd {
        message: summary_msg,
    });
    update_snapshot(
        ctx.snapshot_tx,
        ctx.transcript,
        false,
        None,
        &std::collections::HashSet::new(),
        None,
    );
}

/// 取消终态：发 `stop_reason: Aborted` 的 `TurnEnd`（transcript 不变、不写 session）。
pub(super) fn send_aborted_turn_end(ctx: &RunContext) {
    let _ = ctx.events_tx.send(AgentEvent::TurnEnd {
        message: Arc::new(AssistantMessage {
            content: Vec::new(),
            model: None,
            usage: None,
            stop_reason: Some(StopReason::Aborted),
            error_message: None,
            timestamp: 0,
        }),
        tool_results: Vec::new(),
    });
}

/// 每轮请求前的上下文准备（Task 041 提交纪律 + 一期最终投影）。
///
/// - compactor 启用：`plan_context` 预算检查 + 摘要压缩；仅压缩成功（commit: Some）
///   才提交 transcript/session；取消返回 `Aborted`（终止 run，transcript 原样）；
///   其他失败降级为临时截断（transcript 不写回）。
/// - compactor 关闭：中间投影 = 完整 transcript。
/// - 两条路径统一经一期最终投影（`transform_context` 钩子或 `context_window` 硬上限）。
pub(super) async fn prepare_request_messages(
    ctx: &mut RunContext<'_>,
    signal: &CancellationToken,
) -> PreparedRequest {
    // 克隆 compactor Arc（廉价），避免在 match 内同时持有 ctx.config 的不可变借用
    // 与 commit_compaction 需要的 &mut ctx。
    let compactor_opt = ctx.config.compactor.clone();
    let intermediate = match compactor_opt {
        Some(compactor) => {
            let result = plan_context(
                ctx.transcript.as_slice(),
                &ctx.config.compaction,
                compactor.as_ref(),
                signal.clone(),
            )
            .await;
            match result {
                Err(CompactionError::Cancelled) => return PreparedRequest::Aborted,
                Err(_other) => {
                    // 防御：plan_context 内部已把其他错误降级为临时截断，不应到达。
                    ctx.transcript.clone()
                }
                Ok(prepared) => {
                    if let Some(commit) = prepared.commit {
                        commit_compaction(ctx, commit);
                    }
                    prepared.request_messages
                }
            }
        }
        None => {
            // 无 compactor：中间投影 = 完整 transcript（最终投影统一施加）。
            ctx.transcript.clone()
        }
    };

    // 一期最终投影（单点）：transform_context 钩子（最终权威）或 context_window
    // 硬上限截断。compactor 分支与默认分支统一经过，杜绝绕过回归（Task 041）。
    PreparedRequest::Messages(apply_final_projection(ctx, signal, &intermediate))
}
