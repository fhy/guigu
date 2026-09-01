use super::*;
use crate::core::agent::{AgentConfig, AgentHandle};
use crate::core::event::AgentEvent;
use crate::core::message::{
    AssistantContent, AssistantMessage, Message, StopReason, ThinkingLevel, UserContent,
    UserMessage,
};
use crate::core::provider::{
    AssistantEvent, AssistantStream, Model, ModelProvider, ProviderError, ProviderRequest,
};
use crate::core::runtime::{AgentRuntime, LoopConfig};
use crate::remote::server::RemoteServer;
use async_trait::async_trait;
use futures::stream;
use std::sync::Arc;
use tokio::io::duplex;

/// 最小 provider：单文本 turn。
struct NoopProvider;

#[async_trait]
impl ModelProvider for NoopProvider {
    async fn stream(&self, _request: ProviderRequest) -> Result<AssistantStream, ProviderError> {
        let message = AssistantMessage {
            content: vec![AssistantContent::Text {
                text: "ok".to_string(),
            }],
            model: None,
            usage: None,
            stop_reason: Some(StopReason::Completed),
            error_message: None,
            timestamp: 0,
        };
        Ok(Box::pin(stream::iter(vec![
            AssistantEvent::TextDelta {
                text: "ok".to_string(),
            },
            AssistantEvent::Done { message },
        ])))
    }
}

fn make_handle() -> AgentHandle {
    let provider = Arc::new(NoopProvider);
    let runtime = AgentRuntime {
        provider,
        tools: Vec::new(),
        loop_config: LoopConfig {
            model: Model {
                id: "test-model".to_string(),
                context_window: 8192,
            },
            ..LoopConfig::default()
        },
    };
    AgentHandle::spawn(
        AgentConfig {
            system_prompt: "test".to_string(),
            model: Some("test-model".to_string()),
            thinking_level: ThinkingLevel::Off,
        },
        runtime,
    )
}

fn user_msg(text: &str) -> Message {
    Message::User(UserMessage {
        content: vec![UserContent::Text {
            text: text.to_string(),
        }],
        timestamp: 0,
    })
}

/// 设置 duplex + 对端 server，返回 RemoteClient。
async fn connect_with_server() -> RemoteClient {
    let handle = make_handle();
    let server = Arc::new(RemoteServer::new(handle));
    let (client_stream, server_stream) = duplex(4096);
    let server2 = Arc::clone(&server);
    tokio::spawn(async move {
        let _ = server2.serve(server_stream).await;
    });
    RemoteClient::connect(client_stream).await.expect("connect")
}

/// prompt 返回 Ok。
#[tokio::test]
async fn test_prompt_returns_ok() {
    let client = connect_with_server().await;
    client
        .prompt(vec![user_msg("hello")])
        .await
        .expect("prompt should succeed");
}

/// subscribe() 收到事件（AgentEnd）。
#[tokio::test]
async fn test_subscribe_receives_events() {
    let client = connect_with_server().await;
    let mut events = client.subscribe();
    client
        .prompt(vec![user_msg("hello")])
        .await
        .expect("prompt should succeed");

    let mut got_agent_end = false;
    for _ in 0..100 {
        match events.recv().await {
            Ok(AgentEvent::AgentEnd { .. }) => {
                got_agent_end = true;
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    assert!(got_agent_end, "should receive AgentEnd");
}

/// snapshot() 随服务端推送更新。
#[tokio::test]
async fn test_snapshot_updates() {
    let client = connect_with_server().await;
    // 初始 snapshot 应为空。
    assert!(client.snapshot().messages.is_empty());

    // prompt 后，等待处理完成。
    let mut events = client.subscribe();
    client
        .prompt(vec![user_msg("hello")])
        .await
        .expect("prompt should succeed");
    for _ in 0..100 {
        match events.recv().await {
            Ok(AgentEvent::AgentEnd { .. }) => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }

    // 请求快照（阻塞式，返回最新快照）；本地 watch 缓存同步更新。
    let snap = client.get_snapshot().await.expect("get_snapshot");
    assert!(
        !snap.messages.is_empty(),
        "snapshot should have messages after prompt"
    );
    assert!(
        !client.snapshot().messages.is_empty(),
        "local watch cache should be updated by the Snapshot push"
    );
}

/// 命令错误路径回传为 Command 错误。
#[tokio::test]
async fn test_command_error() {
    let (client_stream, server_stream) = duplex(4096);

    // 客户端连接。
    let client = RemoteClient::connect(client_stream).await.expect("connect");

    // 服务端：读请求，回错误 Response。
    let server_task = tokio::spawn(async move {
        let (read_half, mut write_half) = tokio::io::split(server_stream);
        let mut reader = LineReader::new(read_half);
        // 发初始 Snapshot。
        let _ = write_line(
            &mut write_half,
            &ServerMessage::Snapshot {
                id: 0,
                snapshot: initial_snapshot(),
            },
        )
        .await;
        // 读请求，回错误 Response。
        if let Ok(Some(req)) = reader.next::<RemoteRequest>().await {
            let id = req.id();
            let _ = write_line(
                &mut write_half,
                &ServerMessage::Response {
                    id,
                    result: Err("simulated error".to_string()),
                },
            )
            .await;
        }
    });

    // 客户端发 prompt，应收到 Command 错误。
    let result = client.prompt(vec![user_msg("hello")]).await;
    match result {
        Err(RemoteError::Command(msg)) => {
            assert_eq!(msg, "simulated error");
        }
        other => panic!("expected Command error, got {other:?}"),
    }

    let _ = server_task.await;
}
