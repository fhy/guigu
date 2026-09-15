//! 命令排空与 turn 边界 helper：`Drain` / `drain_commands` / `collect_pending`
//! + snapshot 更新 + 用户消息追加 + stop_reason 映射。
//!
//! 从 `mod.rs` 拆出（Task 040）：主循环模块保持 400 行体量限制内；
//! 这些 helper 被 `step` / `turn` 子模块共享，经 `mod.rs` re-export 保持
//! 既有 `super::` 引用路径不变。

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::core::agent::{AgentCommand, AgentSnapshot};
use crate::core::event::AgentEvent;
use crate::core::message::{Message, StopReason};

use super::RunContext;

/// 一次 drain 的收集结果。
pub(crate) struct Drain {
    pub(crate) aborted: bool,
    pub(crate) shutdown: bool,
    pub(crate) steer: Vec<Message>,
    pub(crate) followup: Vec<Message>,
}

/// 非阻塞排空命令通道：Abort/Shutdown 就地取消 signal，Steer/FollowUp 收集，
/// 其余（Prompt/Continue/Reset）入队待 run 结束后处理。
pub(crate) fn drain_commands(
    rx: &mut mpsc::Receiver<AgentCommand>,
    queue: &mut VecDeque<AgentCommand>,
    signal: &CancellationToken,
) -> Drain {
    let mut drain = Drain {
        aborted: false,
        shutdown: false,
        steer: Vec::new(),
        followup: Vec::new(),
    };
    while let Ok(cmd) = rx.try_recv() {
        match cmd {
            AgentCommand::Abort => {
                drain.aborted = true;
                signal.cancel();
            }
            AgentCommand::Shutdown => {
                drain.shutdown = true;
                signal.cancel();
            }
            AgentCommand::Steer(msg) => drain.steer.push(msg),
            AgentCommand::FollowUp(msg) => drain.followup.push(msg),
            other => queue.push_back(other),
        }
    }
    drain
}

/// 收集待处理的 steering/followUp：先 drain 通道，再从 queue 弹出流式期间
/// re-queue 的 Steer/FollowUp（其余命令保留在 queue）。
pub(crate) fn collect_pending(
    rx: &mut mpsc::Receiver<AgentCommand>,
    queue: &mut VecDeque<AgentCommand>,
    signal: &CancellationToken,
) -> Drain {
    let mut d = drain_commands(rx, queue, signal);
    let mut remaining = VecDeque::new();
    while let Some(cmd) = queue.pop_front() {
        match cmd {
            AgentCommand::Steer(msg) => d.steer.push(msg),
            AgentCommand::FollowUp(msg) => d.followup.push(msg),
            other => remaining.push_back(other),
        }
    }
    queue.extend(remaining);
    d
}

/// 更新 snapshot（transcript / streaming / pending_tool_calls / error）。
pub(crate) fn update_snapshot(
    snapshot_tx: &watch::Sender<AgentSnapshot>,
    transcript: &[Arc<Message>],
    is_streaming: bool,
    streaming_message: Option<Arc<Message>>,
    pending_tool_calls: &HashSet<String>,
    error_message: Option<String>,
) {
    let mut snap = snapshot_tx.borrow().clone();
    snap.messages = transcript.to_vec();
    snap.is_streaming = is_streaming;
    snap.streaming_message = streaming_message;
    snap.pending_tool_calls = pending_tool_calls.clone();
    snap.error_message = error_message;
    let _ = snapshot_tx.send(snap);
}

/// 追加一条用户消息到 transcript 并发事件。
pub(crate) async fn append_user_message(ctx: &mut RunContext<'_>, msg: Message) {
    let arc = Arc::new(msg);
    let _ = ctx.events_tx.send(AgentEvent::MessageStart {
        message: arc.clone(),
    });
    ctx.transcript.push(arc.clone());
    update_snapshot(
        ctx.snapshot_tx,
        ctx.transcript,
        false,
        None,
        &HashSet::new(),
        None,
    );
    let _ = ctx.events_tx.send(AgentEvent::MessageEnd { message: arc });
}

/// stop_reason 映射：取消 → Aborted，其余（流内 Error / 重试耗尽）→ Error。
pub(crate) fn stop_reason_for_error(aborted: bool) -> StopReason {
    if aborted {
        StopReason::Aborted
    } else {
        StopReason::Error
    }
}
