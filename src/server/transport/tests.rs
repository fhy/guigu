//! `serve_connection` / `serve_tcp` 单测（Task 013）。
//!
//! 用 `duplex` 流验证 dispatch 逻辑：CreateSession / ListSessions / SpawnLane +
//! Prompt / GetSnapshot / Shutdown。用内存存储（同步构造，满足 `storage_factory`
//! 同步闭包约束），不依赖真实网络。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use futures::stream;
use tokio::io::duplex;

use super::super::AgentServer;
use super::super::protocol::{ServerMessage, ServerRequest};
use crate::core::agent::AgentConfig;
use crate::core::message::{
    AssistantContent, AssistantMessage, Message, StopReason, ThinkingLevel, UserContent,
    UserMessage,
};
use crate::core::provider::{
    AssistantEvent, AssistantStream, Model, ModelProvider, ProviderError, ProviderRequest,
};
use crate::core::runtime::{AgentRuntime, LoopConfig};
use crate::core::session::{
    NodeId, SessionEntry, SessionError, SessionStorage, SessionTree, SharedSessionStorage, reduce,
};
use crate::remote::codec::{LineReader, write_line};

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

/// 内存存储（测试用，同步构造；满足 `storage_factory` 同步闭包约束）。
struct InMemoryStorage {
    entries: tokio::sync::Mutex<Vec<SessionEntry>>,
    next_id: AtomicU64,
}

impl InMemoryStorage {
    fn new() -> Self {
        Self {
            entries: tokio::sync::Mutex::new(Vec::new()),
            next_id: AtomicU64::new(0),
        }
    }
}

#[async_trait]
impl SessionStorage for InMemoryStorage {
    async fn append(
        &self,
        parent_id: Option<NodeId>,
        message: Message,
    ) -> Result<NodeId, SessionError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.entries.lock().await.push(SessionEntry {
            id,
            parent_id,
            message,
        });
        Ok(id)
    }

    async fn load(&self) -> Result<SessionTree, SessionError> {
        let entries = self.entries.lock().await.clone();
        reduce(entries)
    }

    fn next_id(&self) -> NodeId {
        self.next_id.load(Ordering::SeqCst)
    }
}

fn make_runtime() -> AgentRuntime {
    AgentRuntime {
        provider: Arc::new(NoopProvider),
        tools: Vec::new(),
        loop_config: LoopConfig {
            model: Model {
                id: "test-model".to_string(),
                context_window: 8192,
            },
            ..LoopConfig::default()
        },
    }
}

fn make_config() -> AgentConfig {
    AgentConfig {
        system_prompt: "test".to_string(),
        model: Some("test-model".to_string()),
        thinking_level: ThinkingLevel::Off,
    }
}

/// 建一个带工厂的 server（storage 用内存存储，runtime 用 NoopProvider）。
fn make_server() -> AgentServer {
    let server = AgentServer::new();
    server.with_runtime_factory(|| (make_config(), make_runtime()));
    server.with_storage_factory(|_id| {
        Arc::new(SharedSessionStorage::new(Arc::new(InMemoryStorage::new())))
    });
    server
}

/// 设置 duplex + 对端 server，返回客户端 `DuplexStream`（测试内 split 后使用）。
async fn connect(server: Arc<AgentServer>) -> tokio::io::DuplexStream {
    let (client_stream, server_stream) = duplex(4096);
    let server2 = Arc::clone(&server);
    tokio::spawn(async move {
        let _ = server2.serve_connection(server_stream).await;
    });
    client_stream
}

/// split 客户端流，返回读侧 `LineReader` 与写侧。
fn split_stream(
    stream: tokio::io::DuplexStream,
) -> (
    LineReader<impl tokio::io::AsyncRead + Unpin>,
    impl tokio::io::AsyncWrite + Unpin,
) {
    let (read_half, write_half) = tokio::io::split(stream);
    (LineReader::new(read_half), write_half)
}

/// CreateSession（指定 id）→ 应答 `Ok(session_id)`。
#[tokio::test]
async fn test_create_session() {
    let server = Arc::new(make_server());
    let (mut reader, mut writer) = split_stream(connect(server.clone()).await);

    write_line(
        &mut writer,
        &ServerRequest::CreateSession {
            id: 1,
            session_id: Some("s1".to_string()),
        },
    )
    .await
    .expect("write");

    let msg = reader
        .next::<ServerMessage>()
        .await
        .expect("read")
        .expect("some");
    match msg {
        ServerMessage::Response { id, result } => {
            assert_eq!(id, 1);
            assert_eq!(result, Ok(serde_json::json!("s1")));
        }
        other => panic!("expected Response, got {other:?}"),
    }

    // 验证 session 已注册。
    assert_eq!(server.list_sessions().await, vec!["s1".to_string()]);
}

/// CreateSession（id = None）→ 服务端分配 session_id。
#[tokio::test]
async fn test_create_session_auto_id() {
    let server = Arc::new(make_server());
    let (mut reader, mut writer) = split_stream(connect(server.clone()).await);

    write_line(
        &mut writer,
        &ServerRequest::CreateSession {
            id: 1,
            session_id: None,
        },
    )
    .await
    .expect("write");

    let msg = reader
        .next::<ServerMessage>()
        .await
        .expect("read")
        .expect("some");
    match msg {
        ServerMessage::Response { id, result } => {
            assert_eq!(id, 1);
            // 服务端分配的 id 是非空字符串。
            match result {
                Ok(serde_json::Value::String(sid)) => assert!(!sid.is_empty()),
                other => panic!("expected Ok(String), got {other:?}"),
            }
        }
        other => panic!("expected Response, got {other:?}"),
    }
    assert_eq!(server.list_sessions().await.len(), 1);
}

/// ListSessions → 应答 `SessionList`。
#[tokio::test]
async fn test_list_sessions() {
    let server = Arc::new(make_server());
    // 预创建两个 session。
    server
        .create_session(
            "s1".to_string(),
            Arc::new(SharedSessionStorage::new(Arc::new(InMemoryStorage::new()))),
        )
        .await
        .expect("create s1");
    server
        .create_session(
            "s2".to_string(),
            Arc::new(SharedSessionStorage::new(Arc::new(InMemoryStorage::new()))),
        )
        .await
        .expect("create s2");

    let (mut reader, mut writer) = split_stream(connect(server.clone()).await);
    write_line(&mut writer, &ServerRequest::ListSessions { id: 1 })
        .await
        .expect("write");

    let msg = reader
        .next::<ServerMessage>()
        .await
        .expect("read")
        .expect("some");
    match msg {
        ServerMessage::SessionList { id, sessions } => {
            assert_eq!(id, 1);
            assert_eq!(sessions, vec!["s1".to_string(), "s2".to_string()]);
        }
        other => panic!("expected SessionList, got {other:?}"),
    }
}

/// SpawnLane + Prompt → 应答 `Ok`，且 lane 状态可查（GetSnapshot）。
#[tokio::test]
async fn test_spawn_lane_and_prompt() {
    let server = Arc::new(make_server());
    server
        .create_session(
            "s1".to_string(),
            Arc::new(SharedSessionStorage::new(Arc::new(InMemoryStorage::new()))),
        )
        .await
        .expect("create");

    let (mut reader, mut writer) = split_stream(connect(server.clone()).await);

    // SpawnLane。
    write_line(
        &mut writer,
        &ServerRequest::SpawnLane {
            id: 1,
            session_id: "s1".to_string(),
            lane_id: "l1".to_string(),
        },
    )
    .await
    .expect("write spawn");
    let msg = reader
        .next::<ServerMessage>()
        .await
        .expect("read")
        .expect("some");
    match msg {
        ServerMessage::Response { id, result } => {
            assert_eq!(id, 1);
            assert!(result.is_ok());
        }
        other => panic!("expected Response, got {other:?}"),
    }

    // Prompt。
    write_line(
        &mut writer,
        &ServerRequest::Prompt {
            id: 2,
            session_id: "s1".to_string(),
            lane_id: "l1".to_string(),
            messages: vec![Message::User(UserMessage {
                content: vec![UserContent::Text {
                    text: "hi".to_string(),
                }],
                timestamp: 0,
            })],
        },
    )
    .await
    .expect("write prompt");
    let msg = reader
        .next::<ServerMessage>()
        .await
        .expect("read")
        .expect("some");
    match msg {
        ServerMessage::Response { id, result } => {
            assert_eq!(id, 2);
            assert!(result.is_ok());
        }
        other => panic!("expected Response, got {other:?}"),
    }

    // GetSnapshot：lane 状态可查。
    write_line(
        &mut writer,
        &ServerRequest::GetSnapshot {
            id: 3,
            session_id: "s1".to_string(),
            lane_id: "l1".to_string(),
        },
    )
    .await
    .expect("write get_snapshot");
    let msg = reader
        .next::<ServerMessage>()
        .await
        .expect("read")
        .expect("some");
    match msg {
        ServerMessage::Snapshot {
            session_id,
            lane_id,
            ..
        } => {
            assert_eq!(session_id, "s1");
            assert_eq!(lane_id, "l1");
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }
}

/// Shutdown → 应答 `Ok` 后连接关闭（EOF）。
#[tokio::test]
async fn test_shutdown_closes_connection() {
    let server = Arc::new(make_server());
    let (mut reader, mut writer) = split_stream(connect(server.clone()).await);

    write_line(&mut writer, &ServerRequest::Shutdown { id: 1 })
        .await
        .expect("write");

    let msg = reader
        .next::<ServerMessage>()
        .await
        .expect("read")
        .expect("some");
    match msg {
        ServerMessage::Response { id, result } => {
            assert_eq!(id, 1);
            assert!(result.is_ok());
        }
        other => panic!("expected Response, got {other:?}"),
    }

    // 连接应已关闭（EOF）。
    let eof = reader.next::<ServerMessage>().await.expect("read");
    assert!(eof.is_none(), "expected None after Shutdown");
}

/// 不存在的 session → `Response` 携带 `Err`（不 panic）。
#[tokio::test]
async fn test_unknown_session_error() {
    let server = Arc::new(make_server());
    let (mut reader, mut writer) = split_stream(connect(server.clone()).await);

    write_line(
        &mut writer,
        &ServerRequest::Prompt {
            id: 1,
            session_id: "nope".to_string(),
            lane_id: "l1".to_string(),
            messages: vec![],
        },
    )
    .await
    .expect("write");

    let msg = reader
        .next::<ServerMessage>()
        .await
        .expect("read")
        .expect("some");
    match msg {
        ServerMessage::Response { id, result } => {
            assert_eq!(id, 1);
            assert!(result.is_err(), "expected Err for unknown session");
        }
        other => panic!("expected Response, got {other:?}"),
    }
}
