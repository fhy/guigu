//! Runtime 执行引擎：单 runtime task 的主循环（turn 调度 + 工具编排 +
//! steering/followUp + 取消 + 重试）。
//!
//! 一期行为契约：
//! - **Context window**：每轮请求前按模型 `context_window` 粗估 token，超限保守截断
//!   （或 `transform_context` 钩子覆盖）。
//! - **工具并发**：默认顺序；仅显式 `ReadOnly` 工具可并行；`Exclusive` 独占。
//! - **重试**：仅重试 provider 请求（`stream()` 建立失败），不重试工具；指数退避、可取消。
//! - **取消**：一个 run 一个 `CancellationToken`，贯穿 provider 流、工具、退避等待。
//! - **状态**：agent 内部持有完整 transcript；对外 `AgentSnapshot` 不可变。
//!
//! 模块拆分：`turn`（流消费 + 重试）、`tools`（工具编排）、`step`（turn 收尾）。

mod step;
mod tools;
mod turn;

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::core::agent::{AgentCommand, AgentSnapshot};
use crate::core::context::{ContextBudget, default_convert_to_llm};
use crate::core::event::AgentEvent;
use crate::core::message::{
    AssistantMessage, Message, StopReason, ThinkingLevel, ToolCall, ToolResultMessage,
};
use crate::core::provider::{Context, Model, ModelProvider, ProviderRequest, ToolSpec};
use crate::core::tool::{Tool, ToolResult};

/// 工具执行策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExecutionMode {
    /// 顺序执行（默认）。
    Sequential,
    /// 仅 `ReadOnly` 工具组内并行，其余串行。
    ReadOnlyParallel,
}

/// `transform_context` 钩子：在预算截断位置覆盖（二期接摘要压缩）。
pub type TransformContextHook =
    Box<dyn Fn(Vec<Arc<Message>>, CancellationToken) -> Vec<Arc<Message>> + Send + Sync>;

/// `before_tool_call` 钩子：参数校验/否决。`Err` 表示否决（错误进 ToolResult）。
pub type BeforeToolCallHook =
    Box<dyn Fn(&ToolCall, &serde_json::Value) -> Result<(), String> + Send + Sync>;

/// `after_tool_call` 钩子：可改写工具结果。
pub type AfterToolCallHook = Box<dyn Fn(&ToolCall, ToolResult) -> ToolResult + Send + Sync>;

/// `should_stop_after_turn` 钩子：true 表示本 turn 后停止 loop。
pub type ShouldStopAfterTurnHook =
    Box<dyn Fn(&AssistantMessage, &[ToolResultMessage]) -> bool + Send + Sync>;

/// `prepare_next_turn` 钩子：返回要注入下一 turn 的额外消息。
pub type PrepareNextTurnHook =
    Box<dyn Fn(&AssistantMessage, &[ToolResultMessage]) -> Vec<Message> + Send + Sync>;

/// loop 配置：模型 + 钩子 + 工具执行策略 + 重试参数。
///
/// 含 `Box<dyn Fn>` 钩子，不可 `Clone`（调用方按需持有单一实例）。
pub struct LoopConfig {
    pub model: Model,
    /// transcript（`Arc<Message>`）→ LLM 消息投影。默认 `default_convert_to_llm`。
    pub convert_to_llm: fn(Vec<Arc<Message>>) -> Vec<Message>,
    /// 上下文变换钩子（覆盖默认预算截断）。
    pub transform_context: Option<TransformContextHook>,
    /// 工具调用前钩子（参数校验/否决）。
    pub before_tool_call: Option<BeforeToolCallHook>,
    /// 工具调用后钩子（改写结果）。
    pub after_tool_call: Option<AfterToolCallHook>,
    /// turn 后停止判定。
    pub should_stop_after_turn: Option<ShouldStopAfterTurnHook>,
    /// 下一 turn 准备（注入额外消息）。
    pub prepare_next_turn: Option<PrepareNextTurnHook>,
    /// 工具执行策略。
    pub tool_execution: ToolExecutionMode,
    /// provider 请求最大重试次数（不含首次）。
    pub max_retries: u32,
    /// 重试基础退避（0.5s·2^n 的 0.5s）。
    pub retry_base_delay: Duration,
    /// 重试退避上限。
    pub retry_max_delay: Duration,
}

impl Default for LoopConfig {
    fn default() -> Self {
        LoopConfig {
            model: Model {
                id: "default".to_string(),
                context_window: 8192,
            },
            convert_to_llm: default_convert_to_llm,
            transform_context: None,
            before_tool_call: None,
            after_tool_call: None,
            should_stop_after_turn: None,
            prepare_next_turn: None,
            tool_execution: ToolExecutionMode::Sequential,
            max_retries: 3,
            retry_base_delay: Duration::from_millis(500),
            retry_max_delay: Duration::from_secs(30),
        }
    }
}

/// 运行时依赖：spawn 时注入的 provider + 工具 + loop 配置。
pub struct AgentRuntime {
    pub provider: Arc<dyn ModelProvider>,
    pub tools: Vec<Arc<dyn Tool>>,
    pub loop_config: LoopConfig,
}

/// 单次 run 的上下文：捆绑 loop 需要的可变状态与依赖。
pub(crate) struct RunContext<'a> {
    pub transcript: &'a mut Vec<Arc<Message>>,
    pub queue: &'a mut VecDeque<AgentCommand>,
    pub rx: &'a mut mpsc::Receiver<AgentCommand>,
    pub events_tx: &'a broadcast::Sender<AgentEvent>,
    pub snapshot_tx: &'a watch::Sender<AgentSnapshot>,
    pub provider: &'a Arc<dyn ModelProvider>,
    pub tools: &'a [Arc<dyn Tool>],
    pub config: &'a LoopConfig,
    pub system_prompt: &'a str,
    pub thinking_level: ThinkingLevel,
}

/// run 结果：是否收到 Shutdown + run 期间消费的 Steer/FollowUp 数（已计入 sent）。
pub(crate) struct RunOutcome {
    pub shutdown: bool,
    pub consumed: u64,
}

/// 一次 drain 的收集结果。
struct Drain {
    aborted: bool,
    shutdown: bool,
    steer: Vec<Message>,
    followup: Vec<Message>,
}

/// 非阻塞排空命令通道：Abort/Shutdown 就地取消 signal，Steer/FollowUp 收集，
/// 其余（Prompt/Continue/Reset）入队待 run 结束后处理。
fn drain_commands(
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
fn collect_pending(
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
fn update_snapshot(
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

/// 把工具列表投影为传给 LLM 的 `ToolSpec`。
fn tool_specs(tools: &[Arc<dyn Tool>]) -> Vec<ToolSpec> {
    tools
        .iter()
        .map(|t| ToolSpec {
            name: t.name().to_string(),
            description: t.description().to_string(),
            parameters: t.parameters(),
        })
        .collect()
}

/// 构建本轮 LLM 消息：transform_context 钩子（或默认预算截断）→ convert_to_llm。
///
/// 仅 `transform_context` 钩子消耗所有权时才 clone transcript；默认预算路径
/// 借用 `&[Arc<Message>]`，避免全量浅拷贝。
fn build_llm_messages(ctx: &RunContext, signal: &CancellationToken) -> Vec<Message> {
    let transformed = if let Some(hook) = &ctx.config.transform_context {
        hook(ctx.transcript.clone(), signal.clone())
    } else {
        let budget = ContextBudget::new(ctx.config.model.context_window);
        budget.truncate(ctx.transcript.as_slice())
    };
    (ctx.config.convert_to_llm)(transformed)
}

/// 构建本轮 provider 请求（模型 + 上下文 + 工具 + 取消信号）。
fn build_request(ctx: &RunContext, signal: &CancellationToken) -> ProviderRequest {
    ProviderRequest {
        model: ctx.config.model.clone(),
        context: Context {
            system_prompt: ctx.system_prompt.to_string(),
            messages: build_llm_messages(ctx, signal),
            tools: tool_specs(ctx.tools),
        },
        thinking_level: ctx.thinking_level.clone(),
        session_id: None,
        signal: signal.clone(),
    }
}

/// 运行一次 agent loop（一个 run）。
///
/// `initial` 为本 run 的新用户消息（`Continue` 为空）。返回 `RunOutcome`。
pub(crate) async fn run_agent_loop(ctx: &mut RunContext<'_>, initial: Vec<Message>) -> RunOutcome {
    let signal = CancellationToken::new();
    let mut consumed: u64 = 0;

    // 追加初始用户消息（drain 检查 Abort/Shutdown）。
    let mut shutdown = step::append_initial_messages(ctx, initial, &signal).await;

    let _ = ctx.events_tx.send(AgentEvent::AgentStart);

    loop {
        if shutdown {
            break;
        }

        let _ = ctx.events_tx.send(AgentEvent::TurnStart);

        // 流式消费 assistant 响应（含重试）。
        let request = build_request(ctx, &signal);
        let turn = turn::stream_turn(
            ctx.provider,
            ctx.events_tx,
            ctx.rx,
            ctx.queue,
            &signal,
            request,
            ctx.config,
        )
        .await;

        // assistant 消息入 transcript；TurnEnd 事件需 `Arc<AssistantMessage>`。
        let assistant_msg = turn.assistant_message.clone();
        let assistant_arc = Arc::new(Message::Assistant(assistant_msg.clone()));
        let assistant_msg_arc = Arc::new(assistant_msg);
        ctx.transcript.push(assistant_arc.clone());
        update_snapshot(
            ctx.snapshot_tx,
            ctx.transcript,
            false,
            None,
            &HashSet::new(),
            turn.assistant_message.error_message.clone(),
        );

        // 取消：产出 stop_reason: Aborted 的 TurnEnd 并退出。
        if turn.aborted {
            let _ = ctx.events_tx.send(AgentEvent::TurnEnd {
                message: assistant_msg_arc,
                tool_results: Vec::new(),
            });
            break;
        }

        // 无工具 / 有工具 两条收尾路径。
        let step = if turn.tool_calls.is_empty() {
            step::no_tool_step(ctx, assistant_msg_arc, &signal).await
        } else {
            step::tool_step(ctx, &turn, assistant_msg_arc, &signal).await
        };
        match step {
            step::LoopStep::Break { shutdown: s } => {
                shutdown = s;
                break;
            }
            step::LoopStep::Continue { consumed: c } => consumed += c,
        }
    }

    let _ = ctx.events_tx.send(AgentEvent::AgentEnd {
        messages: ctx.transcript.clone(),
    });

    RunOutcome { shutdown, consumed }
}

/// 追加一条用户消息到 transcript 并发事件。
async fn append_user_message(ctx: &mut RunContext<'_>, msg: Message) {
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
