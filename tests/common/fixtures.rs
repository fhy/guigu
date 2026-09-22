use guigu::core::message::{
    AssistantContent, AssistantMessage, Message, StopReason, ThinkingLevel, ToolCall, UserContent,
    UserMessage,
};
use guigu::core::provider::{AssistantEvent, ModelProvider};
use guigu::core::session::SessionEntry;
use guigu::core::tool::Tool;
use guigu::core::{AgentConfig, AgentRuntime, LoopConfig, Model, ToolExecutionMode};
use std::sync::Arc;
use std::time::Duration;

/// 纯文本 turn 脚本：[TextDelta, Done]。
pub fn text_turn(text: &str) -> Vec<AssistantEvent> {
    let message = AssistantMessage {
        content: vec![AssistantContent::Text {
            text: text.to_string(),
        }],
        model: None,
        usage: None,
        stop_reason: Some(StopReason::Completed),
        error_message: None,
        timestamp: 0,
    };
    vec![
        AssistantEvent::TextDelta {
            text: text.to_string(),
        },
        AssistantEvent::Done { message },
    ]
}

/// 工具调用 turn 脚本：[ToolCallStart, ToolCallEnd, Done]。
pub fn tool_call_turn(id: &str, name: &str, args: &str) -> Vec<AssistantEvent> {
    tool_call_turn_with_stop(id, name, args, StopReason::Completed)
}

/// 指定 `stop_reason` 的 tool call turn（Task 040 Length 保护测试用）。
pub fn tool_call_turn_with_stop(
    id: &str,
    name: &str,
    args: &str,
    stop: StopReason,
) -> Vec<AssistantEvent> {
    let message = AssistantMessage {
        content: vec![AssistantContent::ToolCall(ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: args.to_string(),
        })],
        model: None,
        usage: None,
        stop_reason: Some(stop),
        error_message: None,
        timestamp: 0,
    };
    vec![
        AssistantEvent::ToolCallStart {
            id: id.to_string(),
            name: name.to_string(),
            arguments: args.to_string(),
        },
        AssistantEvent::ToolCallEnd { id: id.to_string() },
        AssistantEvent::Done { message },
    ]
}

/// 多工具调用 turn：所有 toolCall 的 Start/End 事件 + 末尾**单个** `Done`
/// （message 含全部 toolCall）。真实 provider 一个 turn 只发一个 `Done`。
pub fn multi_tool_call_turn(calls: &[(&str, &str, &str)]) -> Vec<AssistantEvent> {
    multi_tool_call_turn_with_stop(calls, &[], StopReason::Completed)
}

/// 多工具调用 turn（指定 `stop_reason`）：每个 toolCall 的 Start/End 事件 +
/// 末尾**单个** `Done`（message 含全部 toolCall）。`delta_ids` 中的 toolCall 经
/// `ToolCallStart`（空参数）+ `ToolCallDelta`（累积完整参数）+ `ToolCallEnd` 形成，
/// 其余用 `ToolCallStart`（完整参数）+ `ToolCallEnd`。用于 Task 040 Length 保护
/// 测试覆盖 delta 累积路径与逐调用生命周期事件。
pub fn multi_tool_call_turn_with_stop(
    calls: &[(&str, &str, &str)],
    delta_ids: &[&str],
    stop: StopReason,
) -> Vec<AssistantEvent> {
    let mut events = Vec::new();
    let mut content = Vec::new();
    for (id, name, args) in calls {
        if delta_ids.contains(id) {
            // delta 路径：Start（空参数）→ Delta（累积完整参数）→ End。
            events.push(AssistantEvent::ToolCallStart {
                id: id.to_string(),
                name: name.to_string(),
                arguments: String::new(),
            });
            events.push(AssistantEvent::ToolCallDelta {
                id: id.to_string(),
                arguments_delta: args.to_string(),
            });
        } else {
            events.push(AssistantEvent::ToolCallStart {
                id: id.to_string(),
                name: name.to_string(),
                arguments: args.to_string(),
            });
        }
        events.push(AssistantEvent::ToolCallEnd { id: id.to_string() });
        content.push(AssistantContent::ToolCall(ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: args.to_string(),
        }));
    }
    events.push(AssistantEvent::Done {
        message: AssistantMessage {
            content,
            model: None,
            usage: None,
            stop_reason: Some(stop),
            error_message: None,
            timestamp: 0,
        },
    });
    events
}

/// 构造测试用 `AgentConfig`。
pub fn make_config() -> AgentConfig {
    AgentConfig {
        system_prompt: "test".to_string(),
        model: Some("test-model".to_string()),
        thinking_level: ThinkingLevel::Off,
    }
}

/// 构造 `AgentRuntime`：注入 provider + 工具 + loop 配置（测试用短退避）。
pub fn make_runtime(
    provider: Arc<dyn ModelProvider>,
    tools: Vec<Arc<dyn Tool>>,
    mode: ToolExecutionMode,
    context_window: u32,
) -> AgentRuntime {
    AgentRuntime {
        provider,
        tools,
        loop_config: LoopConfig {
            model: Model {
                id: "test-model".to_string(),
                context_window,
            },
            tool_execution: mode,
            retry_base_delay: Duration::from_millis(1),
            ..LoopConfig::default()
        },
    }
}

/// 构造 User 文本消息（测试用）。
pub fn user_msg(text: &str) -> Message {
    Message::User(UserMessage {
        content: vec![UserContent::Text {
            text: text.to_string(),
        }],
        timestamp: 0,
    })
}

/// 序列化 entry 为 JSONL 一行（含行尾换行，测试用）。
pub fn line(id: u64, parent: Option<u64>, text: &str) -> String {
    let entry = SessionEntry {
        id,
        parent_id: parent,
        message: user_msg(text),
    };
    format!("{}\n", serde_json::to_string(&entry).unwrap())
}

/// 从 transcript 提取所有 ToolResult 的文本内容（按顺序）。
pub fn tool_result_texts(messages: &[Arc<Message>]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|m| match m.as_ref() {
            Message::ToolResult(tr) => tr.content.iter().find_map(|c| match c {
                guigu::core::message::ToolResultContent::Text { text } => Some(text.clone()),
                _ => None,
            }),
            _ => None,
        })
        .collect()
}

/// 接收事件直到 AgentEnd，带 5s 超时兜底。
pub async fn collect_until_agent_end(
    rx: &mut tokio::sync::broadcast::Receiver<guigu::core::event::AgentEvent>,
) -> Vec<guigu::core::event::AgentEvent> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut events = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!("collect_until_agent_end: timeout before AgentEnd");
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(event)) => {
                let is_end = matches!(event, guigu::core::event::AgentEvent::AgentEnd { .. });
                events.push(event);
                if is_end {
                    return events;
                }
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                panic!("collect_until_agent_end: event channel closed before AgentEnd")
            }
            Err(_) => panic!("collect_until_agent_end: timeout before AgentEnd"),
        }
    }
}

/// 从 broadcast 接收事件直到匹配 predicate，带 5s 超时兜底。
pub async fn wait_event(
    rx: &mut tokio::sync::broadcast::Receiver<guigu::core::event::AgentEvent>,
    mut predicate: impl FnMut(&guigu::core::event::AgentEvent) -> bool,
) -> Result<guigu::core::event::AgentEvent, String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err("wait_event timeout".to_string());
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(event)) if predicate(&event) => return Ok(event),
            Ok(Ok(_)) | Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                return Err("event channel closed".to_string());
            }
            Err(_) => return Err("wait_event timeout".to_string()),
        }
    }
}
