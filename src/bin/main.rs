use guigu::Agent;
use guigu::core::{
    AgentConfig, AgentHandle,
    message::{Message, UserContent, UserMessage},
};
use std::io;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 创建 Agent 配置
    let agent_config = AgentConfig {
        system_prompt: "You are a helpful assistant.".to_string(),
        model: None,
        thinking_level: guigu::core::message::ThinkingLevel::Off,
    };

    // 创建 AgentHandle
    let agent_handle = AgentHandle::spawn(agent_config);

    // 从 stdin 读取输入
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let prompt = input.trim();

    // 创建用户消息
    let user_message = Message::User(UserMessage {
        content: vec![UserContent::Text {
            text: prompt.to_string(),
        }],
        timestamp: 0,
    });

    // 发送 prompt 并等待完成
    agent_handle.prompt(vec![user_message]).await?;
    agent_handle.wait_for_idle().await?;

    // 获取最终的 assistant 消息并打印
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
