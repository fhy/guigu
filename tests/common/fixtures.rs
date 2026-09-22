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

pub fn tool_call_turn(id: &str, name: &str, args: &str) -> Vec<AssistantEvent> {
    tool_call_turn_with_stop(id, name, args, StopReason::Completed)
}

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

pub fn multi_tool_call_turn(calls: &[(&str, &str, &str)]) -> Vec<AssistantEvent> {
    multi_tool_call_turn_with_stop(calls, &[], StopReason::Completed)
}

pub fn multi_tool_call_turn_with_stop(
    calls: &[(&str, &str, &str)],
    delta_ids: &[&str],
    stop: StopReason,
) -> Vec<AssistantEvent> {
    let mut events = Vec::new();
    let mut content = Vec::new();
    for (id, name, args) in calls {
        if delta_ids.contains(id) {
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

pub fn make_config() -> AgentConfig {
    AgentConfig {
        system_prompt: "test".to_string(),
        model: Some("test-model".to_string()),
        thinking_level: ThinkingLevel::Off,
    }
}

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

pub fn user_msg(text: &str) -> Message {
    Message::User(UserMessage {
        content: vec![UserContent::Text {
            text: text.to_string(),
        }],
        timestamp: 0,
    })
}

pub fn line(id: u64, parent: Option<u64>, text: &str) -> String {
    let entry = SessionEntry {
        id,
        parent_id: parent,
        message: user_msg(text),
    };
    format!("{}\n", serde_json::to_string(&entry).unwrap())
}

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
