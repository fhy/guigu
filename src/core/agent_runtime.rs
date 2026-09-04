//! Agent runtime task：唯一状态所有者，处理命令队列并驱动 003 执行引擎。
//!
//! 并发契约：active run 期间收到的 `Prompt`/`Steer`/`FollowUp` 进入同一 FIFO
//! 队列排队（不返回 Busy），待当前 run 结束后按序处理。`Abort` 在 run 内经
//! drain 检测并取消 run 级 `CancellationToken`，被取消的 run 产出
//! `stop_reason: Aborted` 的 TurnEnd 并照常发出 AgentEnd。
//!
//! 同步点：每处理完一条命令（含 run 期间消费的 Steer/FollowUp 与 Reset 丢弃的
//! 排队命令）即递增 `processed` 计数，供 `AgentHandle::wait_for_idle` 以
//! sent/processed 对齐为同步点。

use std::collections::VecDeque;
use std::sync::Arc;

use tokio::sync::{broadcast, mpsc, watch};

use crate::core::agent::{AgentCommand, AgentConfig, AgentSnapshot};
use crate::core::event::AgentEvent;
use crate::core::message::Message;
use crate::core::runtime::{AgentRuntime, RunContext, run_agent_loop};

/// 启动唯一 runtime task，消费命令队列并驱动 agent loop。
///
/// `initial_transcript` 为初始 transcript（session 恢复时注入历史消息；空 = 新会话）。
///
/// 参数为 5 个通道 + config + runtime + initial_transcript（共 8 个）：通道是
/// runtime task 的对外接口（命令/snapshot/事件/计数/退出），捆绑成结构体收益
/// 不大（仅 `AgentHandle::spawn_with_transcript` 单点调用），故显式 allow。
#[allow(clippy::too_many_arguments)]
pub fn spawn_runtime(
    mut rx: mpsc::Receiver<AgentCommand>,
    snapshot_tx: watch::Sender<AgentSnapshot>,
    events_tx: broadcast::Sender<AgentEvent>,
    processed_tx: watch::Sender<u64>,
    exited_tx: watch::Sender<bool>,
    config: AgentConfig,
    runtime: AgentRuntime,
    initial_transcript: Vec<Arc<Message>>,
) {
    tokio::spawn(async move {
        let mut transcript: Vec<Arc<Message>> = initial_transcript;
        let mut queue: VecDeque<AgentCommand> = VecDeque::new();
        let mut processed: u64 = 0;
        let mut state = RuntimeState {
            rx: &mut rx,
            events_tx: &events_tx,
            snapshot_tx: &snapshot_tx,
            transcript: &mut transcript,
            queue: &mut queue,
            runtime: &runtime,
            config: &config,
        };

        loop {
            // 取下一条命令：优先本地队列（run 期间到达的命令，更旧），再取通道，
            // 保证 FIFO 顺序。
            let cmd = if let Some(queued) = state.queue.pop_front() {
                queued
            } else {
                match state.rx.recv().await {
                    Some(cmd) => cmd,
                    None => break,
                }
            };

            let (shutdown, extra) = process_command(&mut state, cmd).await;

            // 本条命令 + run 期间消费的 Steer/FollowUp + Reset 丢弃的排队命令
            // 都计入 processed，使 wait_for_idle 的 sent/processed 对齐。
            processed += 1 + extra;
            let _ = processed_tx.send(processed);

            if shutdown {
                break;
            }
        }

        let _ = exited_tx.send(true);
    });
}

/// runtime task 的共享状态：命令通道、事件/snapshot 通道、transcript、
/// 本地队列与运行时依赖。捆绑后避免命令处理函数参数过多。
struct RuntimeState<'a> {
    rx: &'a mut mpsc::Receiver<AgentCommand>,
    events_tx: &'a broadcast::Sender<AgentEvent>,
    snapshot_tx: &'a watch::Sender<AgentSnapshot>,
    transcript: &'a mut Vec<Arc<Message>>,
    queue: &'a mut VecDeque<AgentCommand>,
    runtime: &'a AgentRuntime,
    config: &'a AgentConfig,
}

/// 处理单条命令。返回 `(shutdown, extra)`：
/// - `shutdown` 为 true 表示收到 Shutdown，主循环应退出；
/// - `extra` 为 run 期间消费的 Steer/FollowUp 数 + Reset 丢弃的排队命令数
///   （这些命令已计入 sent，需补进 processed）。
async fn process_command(state: &mut RuntimeState<'_>, cmd: AgentCommand) -> (bool, u64) {
    match cmd {
        AgentCommand::Prompt(msgs) => run_with_initial(state, msgs).await,
        AgentCommand::Steer(msg) | AgentCommand::FollowUp(msg) => {
            run_with_initial(state, vec![msg]).await
        }
        AgentCommand::Continue => run_with_initial(state, Vec::new()).await,
        AgentCommand::Abort => {
            // 单命令串行处理，此处无 active run，Abort 为空操作。
            (false, 0)
        }
        AgentCommand::Reset => {
            state.transcript.clear();
            let discarded = state.queue.len() as u64;
            state.queue.clear();
            let mut snap = state.snapshot_tx.borrow().clone();
            snap.messages.clear();
            snap.pending_tool_calls.clear();
            snap.error_message = None;
            let _ = state.snapshot_tx.send(snap);
            (false, discarded)
        }
        AgentCommand::Shutdown => (true, 0),
    }
}

/// 以 `initial` 为新用户消息运行一次 agent loop。
async fn run_with_initial(state: &mut RuntimeState<'_>, initial: Vec<Message>) -> (bool, u64) {
    let mut ctx = RunContext {
        transcript: &mut *state.transcript,
        queue: &mut *state.queue,
        rx: &mut *state.rx,
        events_tx: state.events_tx,
        snapshot_tx: state.snapshot_tx,
        provider: &state.runtime.provider,
        tools: &state.runtime.tools,
        config: &state.runtime.loop_config,
        system_prompt: &state.config.system_prompt,
        thinking_level: state.config.thinking_level.clone(),
    };
    let outcome = run_agent_loop(&mut ctx, initial).await;
    (outcome.shutdown, outcome.consumed)
}
