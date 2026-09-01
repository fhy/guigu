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
use futures::{SinkExt, stream};
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{ReadBuf, duplex};
use tokio_util::bytes::BufMut;

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
    make_handle_with_provider(Arc::new(NoopProvider))
}

fn make_handle_with_provider(provider: Arc<dyn ModelProvider>) -> AgentHandle {
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

/// 突发 provider：run 开始后连续发射多个 TextDelta（带短延迟），保持 run
/// 进行中（事件在飞），用于验证连接建立瞬间的事件/快照顺序。
struct BurstProvider;

#[async_trait]
impl ModelProvider for BurstProvider {
    async fn stream(&self, _request: ProviderRequest) -> Result<AssistantStream, ProviderError> {
        let (mut tx, rx) = futures::channel::mpsc::channel::<AssistantEvent>(64);
        tokio::spawn(async move {
            for i in 0..200 {
                if tx
                    .send(AssistantEvent::TextDelta {
                        text: format!("chunk{}", i),
                    })
                    .await
                    .is_err()
                {
                    break; // receiver dropped
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        });
        Ok(Box::pin(rx) as AssistantStream)
    }
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

/// 回归（问题1）：连接建立瞬间有事件在飞，第一条服务端消息仍为初始 Snapshot。
///
/// 场景：run 进行中（事件在飞）时建立连接。修复前 event task 先于初始
/// Snapshot 入队 spawn，可能在 Snapshot 入队前转发事件，导致首条消息为
/// Event；修复后初始 Snapshot 先入队（mpsc FIFO），首条消息恒为 Snapshot。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_initial_snapshot_first_with_in_flight_events() {
    let provider = Arc::new(BurstProvider);
    let handle = make_handle_with_provider(provider);

    // 发 prompt，run 开始并连续发射事件。
    handle.prompt(vec![user_msg("hi")]).await.expect("prompt");

    // 等待 run 进行中（收到若干事件，确保事件在飞）。
    let mut events = handle.subscribe();
    for _ in 0..3 {
        let _ = events.recv().await;
    }

    // 建立连接（run 进行中，事件在飞）。
    let server = Arc::new(RemoteServer::new(handle));
    let (client_stream, server_stream) = duplex(4096);
    let server2 = Arc::clone(&server);
    let server_task = tokio::spawn(async move { server2.serve(server_stream).await });

    let (client_read, mut client_write) = tokio::io::split(client_stream);
    let mut reader = LineReader::new(client_read);

    // 第一条消息必须是初始 Snapshot（id=0），而非 Event。
    let msg = reader
        .next::<ServerMessage>()
        .await
        .expect("read")
        .expect("some");
    match msg {
        ServerMessage::Snapshot { id, .. } => assert_eq!(id, 0),
        other => panic!("expected initial Snapshot first, got {other:?}"),
    }

    // 清理：发 Shutdown 让 server 干净退出。
    write_line(&mut client_write, &RemoteRequest::Shutdown { id: 1 })
        .await
        .expect("write");
    drop(client_write);
    let _ = server_task.await;
}

/// 自定义流：读半返回请求后阻塞（不 EOF）；写半在写入 `max_bytes` 字节后失败。
///
/// 用于测试 writer task 失败场景：初始 Snapshot 写入成功，后续响应写入失败。
/// `duplex` 缓冲满且对端读半关闭时写半会阻塞而非报错，无法触发写失败路径，
/// 故用自定义流精确触发。
struct FailAfterBytesStream {
    read_data: Vec<u8>,
    read_pos: usize,
    bytes_written: usize,
    max_bytes: usize,
}

impl FailAfterBytesStream {
    fn new(read_data: Vec<u8>, max_bytes: usize) -> Self {
        Self {
            read_data,
            read_pos: 0,
            bytes_written: 0,
            max_bytes,
        }
    }
}

impl AsyncRead for FailAfterBytesStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.read_pos >= self.read_data.len() {
            // 阻塞（不 EOF），等待更多数据（永远不会来）。
            return Poll::Pending;
        }
        let n = buf
            .remaining_mut()
            .min(self.read_data.len() - self.read_pos);
        buf.put_slice(&self.read_data[self.read_pos..self.read_pos + n]);
        self.read_pos += n;
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for FailAfterBytesStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.bytes_written >= self.max_bytes {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "write failed",
            )));
        }
        let n = buf.len().min(self.max_bytes - self.bytes_written);
        self.bytes_written += n;
        Poll::Ready(Ok(n))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// 回归（问题3）：writer task 写失败后，服务端读循环立即终止（不再处理请求）。
///
/// 场景：初始 Snapshot 写入成功后，后续响应写入失败（自定义流在写入初始
/// Snapshot 字节数后失败）。修复前 writer 失败后读循环仍继续处理请求，消息
/// 积累在无界 channel；修复后 writer 失败通知主读循环，serve 立即终止。
#[tokio::test]
async fn test_writer_failure_stops_read_loop() {
    let handle = make_handle();
    let initial_snapshot = handle.snapshot(); // 在创建 server 前取快照

    // 序列化 GetSnapshot 请求为 JSON 行（读半返回，供服务端读循环处理）。
    let req = RemoteRequest::GetSnapshot { id: 1 };
    let mut read_data = serde_json::to_vec(&req).expect("serialize");
    read_data.push(b'\n');

    // 计算初始 Snapshot 的长度（含 \n），用于 FailAfterBytesStream。
    let snapshot_msg = ServerMessage::Snapshot {
        id: 0,
        snapshot: initial_snapshot,
    };
    let initial_len = serde_json::to_vec(&snapshot_msg).expect("serialize").len() + 1;

    // 创建自定义流：读半返回 GetSnapshot 请求后阻塞；写半在初始 Snapshot 后失败。
    let stream = FailAfterBytesStream::new(read_data, initial_len);
    let server = Arc::new(RemoteServer::new(handle));
    let server2 = Arc::clone(&server);
    let server_task = tokio::spawn(async move { server2.serve(stream).await });

    // 服务端读循环应在 writer 失败后立即终止（带超时兜底，避免永久挂起）。
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), server_task).await;
    assert!(
        result.is_ok(),
        "server should terminate after writer failure"
    );
}
