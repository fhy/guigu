use super::*;
use crate::core::agent::AgentConfig;
use crate::core::event::AgentEvent;
use crate::core::message::{
    AssistantContent, AssistantMessage, Message, StopReason, ThinkingLevel, UserContent,
    UserMessage,
};
use crate::core::provider::{
    AssistantEvent, AssistantStream, Model, ModelProvider, ProviderError, ProviderRequest,
};
use crate::core::runtime::{AgentRuntime, LoopConfig};
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

/// 连接后收到初始 Snapshot（id = 0）。
#[tokio::test]
async fn test_initial_snapshot_on_connect() {
    let handle = make_handle();
    let server = Arc::new(RemoteServer::new(handle));
    let (client_stream, server_stream) = duplex(4096);

    let server2 = Arc::clone(&server);
    let server_task = tokio::spawn(async move { server2.serve(server_stream).await });

    let (client_read, mut client_write) = tokio::io::split(client_stream);
    let mut reader = LineReader::new(client_read);

    let msg = reader
        .next::<ServerMessage>()
        .await
        .expect("read")
        .expect("some");
    match msg {
        ServerMessage::Snapshot { id, snapshot } => {
            assert_eq!(id, 0);
            assert!(snapshot.messages.is_empty());
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }

    // 发 Shutdown 让 server 干净退出。
    write_line(&mut client_write, &RemoteRequest::Shutdown { id: 1 })
        .await
        .expect("write");
    drop(client_write);
    let _ = server_task.await;
}

/// Prompt 后收到完整事件序列（AgentStart→TurnStart→TurnEnd→AgentEnd）。
#[tokio::test]
async fn test_prompt_triggers_events() {
    let handle = make_handle();
    let server = Arc::new(RemoteServer::new(handle));
    let (client_stream, server_stream) = duplex(4096);

    let server2 = Arc::clone(&server);
    let server_task = tokio::spawn(async move { server2.serve(server_stream).await });

    let (client_read, mut client_write) = tokio::io::split(client_stream);
    let mut reader = LineReader::new(client_read);

    // 读初始 Snapshot。
    let msg = reader
        .next::<ServerMessage>()
        .await
        .expect("read")
        .expect("some");
    assert!(matches!(msg, ServerMessage::Snapshot { .. }));

    // 发 Prompt。
    let prompt = RemoteRequest::Prompt {
        id: 1,
        messages: vec![user_msg("hello")],
    };
    write_line(&mut client_write, &prompt).await.expect("write");

    // 读消息直到 Response + AgentEnd。
    let mut got_response = false;
    let mut events = Vec::new();
    loop {
        let msg = reader
            .next::<ServerMessage>()
            .await
            .expect("read")
            .expect("some");
        match msg {
            ServerMessage::Response { id, result } => {
                assert_eq!(id, 1);
                assert!(result.is_ok());
                got_response = true;
            }
            ServerMessage::Event { event } => {
                events.push(event);
                if matches!(events.last(), Some(AgentEvent::AgentEnd { .. })) {
                    break;
                }
            }
            ServerMessage::Snapshot { .. } => {}
        }
    }
    assert!(got_response, "should have received Response");

    // 验证事件骨架。
    let skeleton: Vec<&str> = events
        .iter()
        .filter_map(|ev| match ev {
            AgentEvent::AgentStart => Some("AgentStart"),
            AgentEvent::AgentEnd { .. } => Some("AgentEnd"),
            AgentEvent::TurnStart => Some("TurnStart"),
            AgentEvent::TurnEnd { .. } => Some("TurnEnd"),
            _ => None,
        })
        .collect();
    assert_eq!(
        skeleton,
        vec!["AgentStart", "TurnStart", "TurnEnd", "AgentEnd"]
    );

    drop(reader);
    drop(client_write);
    let _ = server_task.await;
}

/// GetSnapshot 返回当前快照。
#[tokio::test]
async fn test_get_snapshot() {
    let handle = make_handle();
    let server = Arc::new(RemoteServer::new(handle));
    let (client_stream, server_stream) = duplex(4096);

    let server2 = Arc::clone(&server);
    let server_task = tokio::spawn(async move { server2.serve(server_stream).await });

    let (client_read, mut client_write) = tokio::io::split(client_stream);
    let mut reader = LineReader::new(client_read);

    // 读初始 Snapshot。
    let msg = reader
        .next::<ServerMessage>()
        .await
        .expect("read")
        .expect("some");
    assert!(matches!(msg, ServerMessage::Snapshot { .. }));

    // 发 GetSnapshot。
    let get_snapshot = RemoteRequest::GetSnapshot { id: 5 };
    write_line(&mut client_write, &get_snapshot)
        .await
        .expect("write");

    // 读 Snapshot 应答。
    let msg = reader
        .next::<ServerMessage>()
        .await
        .expect("read")
        .expect("some");
    match msg {
        ServerMessage::Snapshot { id, snapshot } => {
            assert_eq!(id, 5);
            assert!(snapshot.messages.is_empty());
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }

    drop(reader);
    drop(client_write);
    let _ = server_task.await;
}

/// Shutdown 后流关闭。
#[tokio::test]
async fn test_shutdown_closes_stream() {
    let handle = make_handle();
    let server = Arc::new(RemoteServer::new(handle));
    let (client_stream, server_stream) = duplex(4096);

    let server2 = Arc::clone(&server);
    let server_task = tokio::spawn(async move { server2.serve(server_stream).await });

    let (client_read, mut client_write) = tokio::io::split(client_stream);
    let mut reader = LineReader::new(client_read);

    // 读初始 Snapshot。
    let msg = reader
        .next::<ServerMessage>()
        .await
        .expect("read")
        .expect("some");
    assert!(matches!(msg, ServerMessage::Snapshot { .. }));

    // 发 Shutdown。
    let shutdown = RemoteRequest::Shutdown { id: 9 };
    write_line(&mut client_write, &shutdown)
        .await
        .expect("write");

    // 读 Response。
    let msg = reader
        .next::<ServerMessage>()
        .await
        .expect("read")
        .expect("some");
    match msg {
        ServerMessage::Response { id, result } => {
            assert_eq!(id, 9);
            assert!(result.is_ok());
        }
        other => panic!("expected Response, got {other:?}"),
    }

    // 等 server 完成。
    let _ = server_task.await;

    // 流应已关闭。
    let eof = reader.next::<ServerMessage>().await.expect("read");
    assert!(eof.is_none(), "expected None after Shutdown");
}
