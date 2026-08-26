//! 示例 CLI：最小 echo agent（stdin 读一行 → provider 回显 → 打印）。

use std::io;

use async_trait::async_trait;
use futures::stream;
use guigu::core::message::{
    AssistantContent, AssistantMessage, Message, ModelId, StopReason, ThinkingLevel, UserContent,
    UserMessage,
};
use guigu::core::provider::{
    AssistantEvent, AssistantStream, ModelProvider, ProviderError, ProviderRequest,
};
use guigu::core::{Agent, AgentConfig, AgentHandle, AgentRuntime, LoopConfig, Model};

/// 最小 echo provider：回显最后一条用户消息（演示用，非真实 LLM）。
struct EchoProvider;

#[async_trait]
impl ModelProvider for EchoProvider {
    async fn stream(&self, request: ProviderRequest) -> Result<AssistantStream, ProviderError> {
        let text = request
            .context
            .messages
            .iter()
            .rev()
            .find_map(|m| match m {
                Message::User(u) => u.content.iter().find_map(|c| match c {
                    UserContent::Text { text } => Some(text.clone()),
                    _ => None,
                }),
                _ => None,
            })
            .unwrap_or_default();

        let response = format!("Echo: {text}");
        let message = AssistantMessage {
            content: vec![AssistantContent::Text {
                text: response.clone(),
            }],
            model: Some(ModelId(request.model.id.clone())),
            usage: None,
            stop_reason: Some(StopReason::Completed),
            error_message: None,
            timestamp: 0,
        };
        let events = vec![
            AssistantEvent::TextDelta { text: response },
            AssistantEvent::Done { message },
        ];
        Ok(Box::pin(stream::iter(events)))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let agent_config = AgentConfig {
        system_prompt: "You are a helpful assistant that can echo messages.".to_string(),
        model: Some("echo-model".to_string()),
        thinking_level: ThinkingLevel::Off,
    };

    let loop_config = LoopConfig {
        model: Model {
            id: "echo-model".to_string(),
            context_window: 8192,
        },
        ..LoopConfig::default()
    };

    let runtime = AgentRuntime {
        provider: std::sync::Arc::new(EchoProvider),
        tools: Vec::new(),
        loop_config,
    };

    let agent_handle = AgentHandle::spawn(agent_config, runtime);

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let prompt = input.trim();

    let user_message = Message::User(UserMessage {
        content: vec![UserContent::Text {
            text: prompt.to_string(),
        }],
        timestamp: 0,
    });

    agent_handle.prompt(vec![user_message]).await?;
    agent_handle.wait_for_idle().await?;

    let snapshot = agent_handle.snapshot();
    if let Some(assistant_message) = snapshot
        .messages
        .iter()
        .find(|msg| matches!(msg.as_ref(), Message::Assistant(_)))
    {
        println!("Assistant: {:?}", assistant_message);
    } else {
        println!("No assistant message found");
    }

    Ok(())
}
