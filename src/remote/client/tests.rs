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

/// 自定义流：读半返回初始 Snapshot 后阻塞（不 EOF）；写半总是失败。
///
/// 用于测试写 task 失败场景：`duplex` 缓冲满且对端读半关闭时写半会阻塞
/// 而非报错，无法触发写失败路径，故用自定义流精确触发。
struct FailingWriteStream {
    read_data: Vec<u8>,
    read_pos: usize,
}

impl FailingWriteStream {
    fn new(read_data: Vec<u8>) -> Self {
        Self {
            read_data,
            read_pos: 0,
        }
    }
}

impl AsyncRead for FailingWriteStream {
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

impl AsyncWrite for FailingWriteStream {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "write failed",
        )))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// 回归（问题2）：写 task 写失败后，命令立即失败（不等 30s 超时），后续命令
/// 也因 closed 立即失败。
///
/// 用自定义流（写半总是失败）精确触发写失败。
#[tokio::test]
async fn test_write_failure_fails_commands_immediately() {
    // 序列化初始 Snapshot 为 JSON 行（读半返回，供客户端读 task 处理）。
    let snapshot_msg = ServerMessage::Snapshot {
        id: 0,
        snapshot: initial_snapshot(),
    };
    let mut read_data = serde_json::to_vec(&snapshot_msg).expect("serialize");
    read_data.push(b'\n');

    // 创建自定义流：读半返回初始 Snapshot 后阻塞；写半总是失败。
    let stream = FailingWriteStream::new(read_data);
    let client = RemoteClient::connect(stream).await.expect("connect");

    // 发命令（客户端写 task 将尝试写，失败）。应立即失败（不等 30s 超时）。
    let start = std::time::Instant::now();
    let result = client.prompt(vec![user_msg("hello")]).await;
    let elapsed = start.elapsed();
    assert!(result.is_err(), "command should fail after write failure");
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "command should fail immediately, not wait for timeout (elapsed: {elapsed:?})"
    );

    // 后续命令也应立即失败（closed 已标记）。
    let result = client.prompt(vec![user_msg("hello")]).await;
    assert!(
        result.is_err(),
        "subsequent command should fail after closed"
    );
}

/// 回归（问题2）：对端关闭写端（客户端读 task 收到 EOF）后，在途命令立即
/// 失败（不等 30s 超时）。
#[tokio::test]
async fn test_read_eof_fails_inflight_commands_immediately() {
    let (client_stream, server_stream) = duplex(4096);

    // 客户端连接（Arc 共享，供在途命令 task 使用）。
    let client = Arc::new(RemoteClient::connect(client_stream).await.expect("connect"));

    // 拆分 server_stream 为读半和写半。
    let (server_read, mut server_write) = tokio::io::split(server_stream);
    let server_reader = LineReader::new(server_read);

    // 发初始 Snapshot（客户端基线）。
    write_line(
        &mut server_write,
        &ServerMessage::Snapshot {
            id: 0,
            snapshot: initial_snapshot(),
        },
    )
    .await
    .expect("write");

    // 发命令（在途，等待应答）。
    let client_clone = Arc::clone(&client);
    let cmd_task = tokio::spawn(async move { client_clone.prompt(vec![user_msg("hello")]).await });

    // 关闭 server 写半（客户端读 task 收到 EOF）。
    drop(server_write);
    drop(server_reader);

    // 在途命令应立即失败（不等 30s 超时）。
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), cmd_task)
        .await
        .expect("cmd task should finish")
        .expect("cmd task should not panic");
    assert!(
        result.is_err(),
        "inflight command should fail after read EOF"
    );
}

/// 回归（问题1）：写 task 写失败后（`closed=true`），`abort()` 必须返回 `Err`，
/// 不得因 `tx.send()` 仍成功（写 task 尚未退出、`rx` 仍存活）而错误返回 `Ok`。
///
/// 通过持有 `pending` 锁阻塞写 task 失败后的 drain，精确制造「`closed=true`
/// 但 `rx` 仍存活」的竞态窗口：修复前 `abort()` 仅 `tx.send` 成功即返回 `Ok`，
/// 修复后入队后的 `closed` 检查使其返回 `Err`。
#[tokio::test]
async fn test_abort_returns_err_after_write_failure() {
    // 序列化初始 Snapshot 为 JSON 行（读半返回，供客户端读 task 处理）。
    let snapshot_msg = ServerMessage::Snapshot {
        id: 0,
        snapshot: initial_snapshot(),
    };
    let mut read_data = serde_json::to_vec(&snapshot_msg).expect("serialize");
    read_data.push(b'\n');

    // 创建自定义流：读半返回初始 Snapshot 后阻塞；写半总是失败。
    let stream = FailingWriteStream::new(read_data);
    let client = RemoteClient::connect(stream).await.expect("connect");

    // 持有 pending 锁，阻塞写 task 失败后的 drain（使 rx 保持存活）。
    let pending_guard = client.pending.lock().await;

    // 第一次 abort：入队 Abort 请求，触发写 task 写失败（closed=true）。
    // 其结果取决于时序（写 task 可能已失败），此处仅用于触发，不断言。
    let _ = client.abort();

    // 等待写 task 设置 closed=true（写失败后、drain 前；drain 被锁阻塞）。
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if *client.closed.borrow() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "closed should be set after write failure"
        );
        tokio::task::yield_now().await;
    }

    // 第二次 abort：closed=true 但 rx 仍存活（写 task 被 pending 锁阻塞）。
    // 修复前：tx.send() 成功 → 错误返回 Ok。修复后：入队后 closed 检查 → 返回 Err。
    let second = client.abort();
    assert!(
        second.is_err(),
        "abort should return Err after write failure (closed=true), got {second:?}"
    );

    // 释放 pending 锁，写 task 完成 drain 并退出。
    drop(pending_guard);
}
