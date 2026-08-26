//! turn 收尾步骤：初始消息追加 + 无工具/有工具两条收尾路径。
//!
//! 从 `run_agent_loop` 主循环拆出，控制流用 `LoopStep` 表达（退出/继续）。

use std::collections::HashSet;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::core::agent::AgentCommand;
use crate::core::event::AgentEvent;
use crate::core::message::{AssistantMessage, Message};

use super::{RunContext, append_user_message, collect_pending, drain_commands, tools};

/// 主循环单步结果：退出（带 shutdown 标志）或继续（带本步消费的 Steer/FollowUp 数）。
pub(super) enum LoopStep {
    Break { shutdown: bool },
    Continue { consumed: u64 },
}

/// 追加初始用户消息（逐条 MessageStart/MessageEnd，保留 001 逐消息包裹语义），
/// 每条后 drain 检查 Abort/Shutdown。返回是否收到 Shutdown。
pub(super) async fn append_initial_messages(
    ctx: &mut RunContext<'_>,
    initial: Vec<Message>,
    signal: &CancellationToken,
) -> bool {
    let mut shutdown = false;
    for msg in initial {
        append_user_message(ctx, msg).await;
        // drain 检查 Abort/Shutdown；Steer/FollowUp re-queue（留待 no-tool 边界注入）。
        let d = drain_commands(ctx.rx, ctx.queue, signal);
        for msg in d.steer {
            ctx.queue.push_back(AgentCommand::Steer(msg));
        }
        for msg in d.followup {
            ctx.queue.push_back(AgentCommand::FollowUp(msg));
        }
        if d.aborted || d.shutdown {
            shutdown = d.shutdown;
            break;
        }
    }
    shutdown
}

/// 无工具 turn 收尾：TurnEnd → steering? → followUp? → 退出/继续。
pub(super) async fn no_tool_step(
    ctx: &mut RunContext<'_>,
    assistant_msg_arc: Arc<AssistantMessage>,
    signal: &CancellationToken,
) -> LoopStep {
    let _ = ctx.events_tx.send(AgentEvent::TurnEnd {
        message: assistant_msg_arc,
        tool_results: Vec::new(),
    });
    let d = collect_pending(ctx.rx, ctx.queue, signal);
    let consumed = d.steer.len() as u64 + d.followup.len() as u64;
    if d.aborted || d.shutdown {
        return LoopStep::Break {
            shutdown: d.shutdown,
        };
    }
    if !d.steer.is_empty() {
        // 注入 steering，followUp 留待下轮（入队）。
        for msg in d.followup {
            ctx.queue.push_back(AgentCommand::FollowUp(msg));
        }
        for msg in d.steer {
            append_user_message(ctx, msg).await;
        }
        return LoopStep::Continue { consumed };
    }
    if !d.followup.is_empty() {
        for msg in d.followup {
            append_user_message(ctx, msg).await;
        }
        return LoopStep::Continue { consumed };
    }
    LoopStep::Break { shutdown: false }
}

/// 有工具 turn 收尾：执行工具 → 结果入上下文 → TurnEnd → 钩子 → drain。
pub(super) async fn tool_step(
    ctx: &mut RunContext<'_>,
    turn: &super::turn::TurnResult,
    assistant_msg_arc: Arc<AssistantMessage>,
    signal: &CancellationToken,
) -> LoopStep {
    // prepare → execute → after_tool_call → 结果入上下文。
    let tool_results = tools::execute_tool_calls(ctx, &turn.tool_calls, signal).await;

    for tr in &tool_results {
        let arc = Arc::new(Message::ToolResult(tr.clone()));
        ctx.transcript.push(arc.clone());
    }
    super::update_snapshot(
        ctx.snapshot_tx,
        ctx.transcript,
        false,
        None,
        &HashSet::new(),
        None,
    );
    let _ = ctx.events_tx.send(AgentEvent::TurnEnd {
        message: assistant_msg_arc,
        tool_results: tool_results.clone(),
    });

    // should_stop_after_turn 钩子。
    if let Some(hook) = &ctx.config.should_stop_after_turn
        && hook(&turn.assistant_message, &tool_results)
    {
        return LoopStep::Break { shutdown: false };
    }
    // prepare_next_turn 钩子：注入额外消息。
    if let Some(hook) = &ctx.config.prepare_next_turn {
        for msg in hook(&turn.assistant_message, &tool_results) {
            append_user_message(ctx, msg).await;
        }
    }

    let d = drain_commands(ctx.rx, ctx.queue, signal);
    if d.aborted || d.shutdown {
        return LoopStep::Break {
            shutdown: d.shutdown,
        };
    }
    // 工具路径：steering/followUp 延迟到 run 结束后作为新 run 处理（入队，
    // 不在此计数——主循环处理时再计入 processed）。
    for msg in d.steer {
        ctx.queue.push_back(AgentCommand::Steer(msg));
    }
    for msg in d.followup {
        ctx.queue.push_back(AgentCommand::FollowUp(msg));
    }
    LoopStep::Continue { consumed: 0 }
}
