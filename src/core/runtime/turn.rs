//! turn：流式消费 assistant 响应（含 provider 请求重试）+ 交错 drain（取消）。
//!
//! 重试仅针对 `provider.stream()` 建立失败（外层 `Err`），指数退避
//! （`retry_base_delay·2^n`，上限 `retry_max_delay`，可取消）。流内失败
//! （`AssistantEvent::Error`）不重试，产出终态 `AssistantMessage`。

use std::collections::VecDeque;
use std::sync::Arc;

use futures::StreamExt;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::core::agent::AgentCommand;
use crate::core::event::AgentEvent;
use crate::core::message::{
    AssistantContent, AssistantMessage, Message, ModelId, StopReason, ToolCall,
};
use crate::core::provider::{
    AssistantEvent, AssistantStream, ModelProvider, ProviderError, ProviderRequest,
};

use super::{LoopConfig, drain_commands};

/// 一个 turn 的结果。
pub(crate) struct TurnResult {
    pub assistant_message: AssistantMessage,
    pub tool_calls: Vec<ToolCall>,
    pub aborted: bool,
}

/// 建立 provider 流（含重试）。仅重试 `stream()` 建立失败。
async fn stream_with_retry(
    provider: &Arc<dyn ModelProvider>,
    request: &ProviderRequest,
    signal: &CancellationToken,
    config: &LoopConfig,
) -> Result<AssistantStream, ProviderError> {
    let mut attempt: u32 = 0;
    loop {
        match provider.stream(request.clone()).await {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                if signal.is_cancelled() {
                    return Err(ProviderError::Aborted);
                }
                if attempt >= config.max_retries {
                    return Err(e);
                }
                let factor = 2f64.powi(attempt as i32);
                let delay = config
                    .retry_base_delay
                    .mul_f64(factor)
                    .min(config.retry_max_delay);
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = signal.cancelled() => return Err(ProviderError::Aborted),
                }
                attempt += 1;
            }
        }
    }
}

/// 从累积缓冲构建 assistant 消息（用于流式 MessageUpdate）。
fn build_assistant(
    text: &str,
    thinking: &str,
    tool_calls: &[ToolCall],
    current_tool: &Option<ToolCall>,
    model_id: &str,
) -> AssistantMessage {
    let mut content = Vec::new();
    if !thinking.is_empty() {
        content.push(AssistantContent::Thinking {
            text: thinking.to_string(),
        });
    }
    if !text.is_empty() {
        content.push(AssistantContent::Text {
            text: text.to_string(),
        });
    }
    for tc in tool_calls {
        content.push(AssistantContent::ToolCall(tc.clone()));
    }
    if let Some(tc) = current_tool {
        content.push(AssistantContent::ToolCall(tc.clone()));
    }
    AssistantMessage {
        content,
        model: Some(ModelId(model_id.to_string())),
        usage: None,
        stop_reason: None,
        error_message: None,
        timestamp: 0,
    }
}

/// 从 assistant 消息提取 tool_calls（权威来源）。
fn extract_tool_calls(msg: &AssistantMessage) -> Vec<ToolCall> {
    msg.content
        .iter()
        .filter_map(|c| match c {
            AssistantContent::ToolCall(tc) => Some(tc.clone()),
            _ => None,
        })
        .collect()
}

/// turn 流式累积状态：text/thinking/tool_calls + 终态信息。
struct TurnAccumulator {
    text: String,
    thinking: String,
    tool_calls: Vec<ToolCall>,
    current_tool: Option<ToolCall>,
    final_message: Option<AssistantMessage>,
    error_message: Option<String>,
    aborted: bool,
}

impl TurnAccumulator {
    fn new() -> Self {
        TurnAccumulator {
            text: String::new(),
            thinking: String::new(),
            tool_calls: Vec::new(),
            current_tool: None,
            final_message: None,
            error_message: None,
            aborted: false,
        }
    }

    /// 处理单个流事件，返回是否终态（`Done`/`Error`）。
    fn handle_event(&mut self, event: &AssistantEvent) -> bool {
        match event {
            AssistantEvent::TextDelta { text: delta } => {
                self.text.push_str(delta);
                false
            }
            AssistantEvent::ThinkingDelta { thinking: delta } => {
                self.thinking.push_str(delta);
                false
            }
            AssistantEvent::ToolCallStart {
                id,
                name,
                arguments,
            } => {
                if let Some(tc) = self.current_tool.take() {
                    self.tool_calls.push(tc);
                }
                self.current_tool = Some(ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                });
                false
            }
            AssistantEvent::ToolCallDelta {
                id,
                arguments_delta,
            } => {
                if let Some(tc) = &mut self.current_tool
                    && tc.id == *id
                {
                    tc.arguments.push_str(arguments_delta);
                }
                false
            }
            AssistantEvent::ToolCallEnd { id } => {
                if let Some(tc) = &self.current_tool
                    && tc.id == *id
                    && let Some(done) = self.current_tool.take()
                {
                    self.tool_calls.push(done);
                }
                false
            }
            AssistantEvent::Done { message } => {
                self.final_message = Some(message.clone());
                true
            }
            AssistantEvent::Error {
                message,
                aborted: is_aborted,
            } => {
                self.error_message = Some(message.clone());
                self.aborted = *is_aborted;
                true
            }
        }
    }

    /// 构建当前累积的 assistant 消息（用于流式 MessageUpdate）。
    fn build_assistant(&self, model_id: &str) -> AssistantMessage {
        build_assistant(
            &self.text,
            &self.thinking,
            &self.tool_calls,
            &self.current_tool,
            model_id,
        )
    }

    /// 收尾：合并未结束的 current_tool，产出终态 assistant 消息。
    fn finalize(&mut self, model_id: &str) -> AssistantMessage {
        if let Some(tc) = self.current_tool.take() {
            self.tool_calls.push(tc);
        }
        if let Some(msg) = self.final_message.clone() {
            return msg;
        }
        let mut msg = build_assistant(
            &self.text,
            &self.thinking,
            &self.tool_calls,
            &None,
            model_id,
        );
        if self.aborted {
            msg.stop_reason = Some(StopReason::Aborted);
        } else if let Some(err) = self.error_message.clone() {
            msg.stop_reason = Some(StopReason::Error);
            msg.error_message = Some(err);
        } else {
            msg.stop_reason = Some(StopReason::Completed);
        }
        msg
    }
}

/// 流建立失败（重试耗尽/取消）→ 终态 `TurnResult`。
fn stream_failure_result(
    model_id: &str,
    e: ProviderError,
    signal: &CancellationToken,
) -> TurnResult {
    let aborted = matches!(e, ProviderError::Aborted) || signal.is_cancelled();
    let (stop_reason, error_message) = if aborted {
        (Some(StopReason::Aborted), None)
    } else {
        (Some(StopReason::Error), Some(e.to_string()))
    };
    let msg = AssistantMessage {
        content: Vec::new(),
        model: Some(ModelId(model_id.to_string())),
        usage: None,
        stop_reason,
        error_message,
        timestamp: 0,
    };
    TurnResult {
        assistant_message: msg,
        tool_calls: Vec::new(),
        aborted,
    }
}

/// 流式消费一个 turn：建立流（重试）→ 逐事件累积 → 产出 `TurnResult`。
pub(crate) async fn stream_turn(
    provider: &Arc<dyn ModelProvider>,
    events_tx: &broadcast::Sender<AgentEvent>,
    rx: &mut mpsc::Receiver<AgentCommand>,
    queue: &mut VecDeque<AgentCommand>,
    signal: &CancellationToken,
    request: ProviderRequest,
    config: &LoopConfig,
) -> TurnResult {
    let model_id = request.model.id.clone();

    // 建立流（重试）。失败（重试耗尽/取消）→ 终态消息。
    let stream = match stream_with_retry(provider, &request, signal, config).await {
        Ok(s) => s,
        Err(e) => return stream_failure_result(&model_id, e, signal),
    };

    let mut acc = TurnAccumulator::new();

    // MessageStart（初始空 assistant 消息）。
    let initial = Arc::new(Message::Assistant(AssistantMessage {
        content: Vec::new(),
        model: Some(ModelId(model_id.clone())),
        usage: None,
        stop_reason: None,
        error_message: None,
        timestamp: 0,
    }));
    let _ = events_tx.send(AgentEvent::MessageStart { message: initial });

    let mut stream = stream;
    while let Some(event) = stream.next().await {
        // 交错 drain：Abort/Shutdown 就地取消并停止消费。
        // Steer/FollowUp 在此不消费（留待 no-tool 边界注入），re-queue 防丢失。
        let d = drain_commands(rx, queue, signal);
        for msg in d.steer {
            queue.push_back(AgentCommand::Steer(msg));
        }
        for msg in d.followup {
            queue.push_back(AgentCommand::FollowUp(msg));
        }
        if d.aborted || d.shutdown {
            acc.aborted = true;
            break;
        }

        let terminal = acc.handle_event(&event);
        if terminal {
            break;
        }

        // MessageUpdate：累积消息 + 增量 delta。
        let current = acc.build_assistant(&model_id);
        let _ = events_tx.send(AgentEvent::MessageUpdate {
            message: Arc::new(Message::Assistant(current)),
            assistant_event: event,
        });
    }

    // 收尾：终态消息 + tool_calls + MessageEnd。
    let assistant_message = acc.finalize(&model_id);
    let tool_calls = extract_tool_calls(&assistant_message);
    let _ = events_tx.send(AgentEvent::MessageEnd {
        message: Arc::new(Message::Assistant(assistant_message.clone())),
    });

    TurnResult {
        assistant_message,
        tool_calls,
        aborted: acc.aborted,
    }
}
