//! Agent runtime task：唯一状态所有者，处理命令队列。
//!
//! 并发契约：active run 期间收到的 `Prompt`/`Steer`/`FollowUp` 一律进入同一
//! FIFO 队列排队（不返回 Busy），待当前 run 结束后按序处理。
//! `Abort` 在消息边界经 `try_recv` 检测，被取消的 run 产出
//! `stop_reason: Aborted` 的 TurnEnd 并照常发出 AgentEnd。
//!
//! 同步点：每处理完一条命令（含 Reset 丢弃的排队命令）即递增 `processed`
//! 计数，供 `AgentHandle::wait_for_idle` 以 sent/processed 对齐为同步点。

use crate::core::agent::{AgentCommand, AgentConfig, AgentSnapshot};
use crate::core::event::AgentEvent;
use crate::core::message::{AssistantMessage, Message, StopReason};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, watch};

/// 启动唯一 runtime task，消费命令队列并维护 transcript / 队列 / 事件流。
pub fn spawn_runtime(
    mut rx: mpsc::Receiver<AgentCommand>,
    snapshot_tx: watch::Sender<AgentSnapshot>,
    events_tx: broadcast::Sender<AgentEvent>,
    processed_tx: watch::Sender<u64>,
    exited_tx: watch::Sender<bool>,
    _config: AgentConfig,
) {
    tokio::spawn(async move {
        let mut transcript: Vec<Arc<Message>> = Vec::new();
        let mut queue: VecDeque<AgentCommand> = VecDeque::new();
        let mut processed: u64 = 0;

        loop {
            // 取下一条命令：优先本地队列（run 期间到达的命令，更旧），再取通道，
            // 保证 FIFO 顺序。
            let cmd = if let Some(queued) = queue.pop_front() {
                queued
            } else {
                match rx.recv().await {
                    Some(cmd) => cmd,
                    None => break,
                }
            };

            let (shutdown, extra) = process_command(
                cmd,
                &mut rx,
                &events_tx,
                &snapshot_tx,
                &mut transcript,
                &mut queue,
            )
            .await;

            // 本条命令 + Reset 丢弃的排队命令都计入 processed，
            // 使 wait_for_idle 的 sent/processed 对齐。
            processed += 1 + extra;
            let _ = processed_tx.send(processed);

            if shutdown {
                break;
            }
        }

        let _ = exited_tx.send(true);
    });
}

/// 处理单条命令。返回 `(shutdown, extra)`：
/// - `shutdown` 为 true 表示收到 Shutdown，主循环应退出；
/// - `extra` 为 Reset 丢弃的排队命令数（这些命令已计入 sent，需补进 processed）。
async fn process_command(
    cmd: AgentCommand,
    rx: &mut mpsc::Receiver<AgentCommand>,
    events_tx: &broadcast::Sender<AgentEvent>,
    snapshot_tx: &watch::Sender<AgentSnapshot>,
    transcript: &mut Vec<Arc<Message>>,
    queue: &mut VecDeque<AgentCommand>,
) -> (bool, u64) {
    match cmd {
        AgentCommand::Prompt(msgs) => (
            process_run(msgs, rx, events_tx, snapshot_tx, transcript, queue).await,
            0,
        ),
        AgentCommand::Steer(msg) | AgentCommand::FollowUp(msg) => (
            process_run(vec![msg], rx, events_tx, snapshot_tx, transcript, queue).await,
            0,
        ),
        AgentCommand::Continue => {
            // 空 turn：完整 run 事件序列，无消息。
            let _ = events_tx.send(AgentEvent::AgentStart);
            let _ = events_tx.send(AgentEvent::TurnStart);
            let _ = events_tx.send(AgentEvent::TurnEnd {
                message: Arc::new(empty_assistant()),
                tool_results: Vec::new(),
            });
            let _ = events_tx.send(AgentEvent::AgentEnd {
                messages: transcript.clone(),
            });
            (false, 0)
        }
        AgentCommand::Abort => {
            // 单命令串行处理，此处无 active run，Abort 为空操作。
            (false, 0)
        }
        AgentCommand::Reset => {
            transcript.clear();
            let discarded = queue.len() as u64;
            queue.clear();
            let mut snap = snapshot_tx.borrow().clone();
            snap.messages.clear();
            snap.pending_tool_calls.clear();
            snap.error_message = None;
            let _ = snapshot_tx.send(snap);
            (false, discarded)
        }
        AgentCommand::Shutdown => (true, 0),
    }
}

/// 处理一个 run：发 AgentStart/TurnStart，逐条消息发 MessageStart/MessageEnd
/// 并追加 transcript，最后发 TurnEnd/AgentEnd。
///
/// 每条消息边界通过 `drain_pending` 检测 Abort/Shutdown 等命令。
/// 返回 true 表示收到 Shutdown，调用方应退出主循环。
async fn process_run(
    msgs: Vec<Message>,
    rx: &mut mpsc::Receiver<AgentCommand>,
    events_tx: &broadcast::Sender<AgentEvent>,
    snapshot_tx: &watch::Sender<AgentSnapshot>,
    transcript: &mut Vec<Arc<Message>>,
    queue: &mut VecDeque<AgentCommand>,
) -> bool {
    let _ = events_tx.send(AgentEvent::AgentStart);
    let _ = events_tx.send(AgentEvent::TurnStart);
    let mut aborted = false;
    let mut shutdown = false;

    drain_pending(rx, queue, &mut aborted, &mut shutdown);

    for msg in msgs {
        if aborted {
            break;
        }
        drain_pending(rx, queue, &mut aborted, &mut shutdown);
        if aborted {
            break;
        }

        let arc_msg = Arc::new(msg.clone());
        let _ = events_tx.send(AgentEvent::MessageStart {
            message: arc_msg.clone(),
        });
        transcript.push(arc_msg.clone());
        let mut snap = snapshot_tx.borrow().clone();
        snap.messages = transcript.clone();
        let _ = snapshot_tx.send(snap);
        let _ = events_tx.send(AgentEvent::MessageEnd { message: arc_msg });
        tokio::task::yield_now().await;
    }

    let stop_reason = if aborted {
        Some(StopReason::Aborted)
    } else {
        None
    };
    let _ = events_tx.send(AgentEvent::TurnEnd {
        message: Arc::new(AssistantMessage {
            content: Vec::new(),
            model: None,
            usage: None,
            stop_reason,
            error_message: None,
            timestamp: 0,
        }),
        tool_results: Vec::new(),
    });
    let _ = events_tx.send(AgentEvent::AgentEnd {
        messages: transcript.clone(),
    });

    shutdown
}

/// 非阻塞排空通道中的待处理命令，更新 aborted / shutdown 标志。
/// Abort/Shutdown 就地结算（不入队），其余命令入队待当前 run 结束后处理。
fn drain_pending(
    rx: &mut mpsc::Receiver<AgentCommand>,
    queue: &mut VecDeque<AgentCommand>,
    aborted: &mut bool,
    shutdown: &mut bool,
) {
    while let Ok(cmd) = rx.try_recv() {
        match cmd {
            AgentCommand::Abort => *aborted = true,
            AgentCommand::Shutdown => *shutdown = true,
            other => queue.push_back(other),
        }
    }
}

fn empty_assistant() -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        model: None,
        usage: None,
        stop_reason: None,
        error_message: None,
        timestamp: 0,
    }
}
