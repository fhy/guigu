//! 示例 CLI：最小 echo agent（stdin 读一行 → 两轮回放 → 打印最终 assistant 文本）。
//!
//! 一期不接真实 HTTP：默认用 fake provider 做**两轮回放**（工具轮 + 文本轮），
//! 真实 provider 待 adapters 任务落地。入口固定 `cargo run --bin guigu`。

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures::stream;
use guigu::core::message::{
    AssistantContent, AssistantMessage, Message, ModelId, StopReason, ThinkingLevel, ToolCall,
    UserContent, UserMessage,
};
use guigu::core::provider::{
    AssistantEvent, AssistantStream, ModelProvider, ProviderError, ProviderRequest,
};
use guigu::core::{Agent, AgentConfig, AgentHandle, AgentRuntime, LoopConfig, Model};
use guigu::tools::EchoTool;

/// 两轮回放 fake provider（演示用，非真实 LLM）：
/// - 第 1 轮：发出 `echo` 工具调用（回显最后一条用户消息）
/// - 第 2 轮：产出终态文本 `echo: <input>`
struct TwoTurnEchoProvider {
    call_index: AtomicUsize,
}

/// 从请求上下文提取最后一条用户文本消息。
fn last_user_text(request: &ProviderRequest) -> String {
    request
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
        .unwrap_or_default()
}

#[async_trait]
impl ModelProvider for TwoTurnEchoProvider {
    async fn stream(&self, request: ProviderRequest) -> Result<AssistantStream, ProviderError> {
        let idx = self.call_index.fetch_add(1, Ordering::SeqCst);
        let input = last_user_text(&request);
        let model_id = ModelId(request.model.id.clone());

        if idx == 0 {
            // 工具轮：echo 工具调用。
            let args = serde_json::json!({ "message": input }).to_string();
            let message = AssistantMessage {
                content: vec![AssistantContent::ToolCall(ToolCall {
                    id: "call1".to_string(),
                    name: "echo".to_string(),
                    arguments: args.clone(),
                })],
                model: Some(model_id),
                usage: None,
                stop_reason: Some(StopReason::Completed),
                error_message: None,
                timestamp: 0,
            };
            let events = vec![
                AssistantEvent::ToolCallStart {
                    id: "call1".to_string(),
                    name: "echo".to_string(),
                    arguments: args,
                },
                AssistantEvent::ToolCallEnd {
                    id: "call1".to_string(),
                },
                AssistantEvent::Done { message },
            ];
            Ok(Box::pin(stream::iter(events)))
        } else {
            // 文本轮：终态回复。
            let text = format!("echo: {input}");
            let message = AssistantMessage {
                content: vec![AssistantContent::Text { text: text.clone() }],
                model: Some(model_id),
                usage: None,
                stop_reason: Some(StopReason::Completed),
                error_message: None,
                timestamp: 0,
            };
            let events = vec![
                AssistantEvent::TextDelta { text },
                AssistantEvent::Done { message },
            ];
            Ok(Box::pin(stream::iter(events)))
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 从 stdin 读一行。
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let prompt = input.trim();
    if prompt.is_empty() {
        eprintln!("usage: pipe a line on stdin, e.g. `echo hello | cargo run --bin guigu`");
        return Ok(());
    }

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
        provider: Arc::new(TwoTurnEchoProvider {
            call_index: AtomicUsize::new(0),
        }),
        tools: vec![Arc::new(EchoTool)],
        loop_config,
    };

    let handle = AgentHandle::spawn(agent_config, runtime);

    let user_message = Message::User(UserMessage {
        content: vec![UserContent::Text {
            text: prompt.to_string(),
        }],
        timestamp: 0,
    });

    handle.prompt(vec![user_message]).await?;
    handle.wait_for_idle().await?;

    // 打印最终 assistant 文本。
    let snapshot = handle.snapshot();
    let final_text = snapshot
        .messages
        .iter()
        .rev()
        .find_map(|m| match m.as_ref() {
            Message::Assistant(a) => a.content.iter().find_map(|c| match c {
                AssistantContent::Text { text } => Some(text.clone()),
                _ => None,
            }),
            _ => None,
        });
    match final_text {
        Some(text) => println!("{text}"),
        None => eprintln!("no assistant text produced"),
    }

    Ok(())
}
