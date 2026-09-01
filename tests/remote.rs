//! Task 010 集成测试：duplex loopback 端到端。
//!
//! - client `prompt` → server 驱动 runtime → client 收到完整事件序列与更新后的 snapshot
//! - `abort` 非阻塞
//! - `reset` 后 snapshot.messages 清空
//!
//! 同步点：事件序列（`AgentEnd`）+ 阻塞式 `get_snapshot` 轮询（带 deadline），
//! 不用 `sleep` 作为同步点（轮询中的短 sleep 仅为限流，非判定依据）。

mod common;

use std::sync::Arc;
use std::time::Duration;

use guigu::core::ToolExecutionMode;
use guigu::core::event::AgentEvent;
use guigu::core::message::{Message, UserContent, UserMessage};
use guigu::core::provider::ModelProvider;
use guigu::{AgentHandle, RemoteClient, RemoteServer};
use tokio::io::duplex;

fn user_msg(text: &str) -> Message {
    Message::User(UserMessage {
        content: vec![UserContent::Text {
            text: text.to_string(),
        }],
        timestamp: 0,
    })
}

/// 设置 duplex + 对端 server，返回 RemoteClient。
async fn connect_with_server(provider: Arc<dyn ModelProvider>) -> RemoteClient {
    let handle = AgentHandle::spawn(
        common::make_config(),
        common::make_runtime(provider, vec![], ToolExecutionMode::Sequential, 8192),
    );
    let server = Arc::new(RemoteServer::new(handle));
    let (client_stream, server_stream) = duplex(4096);
    let server2 = Arc::clone(&server);
    tokio::spawn(async move {
        let _ = server2.serve(server_stream).await;
    });
    RemoteClient::connect(client_stream).await.expect("connect")
}

/// 读事件直到 AgentEnd，返回事件列表。
async fn read_until_agent_end(
    mut events: tokio::sync::broadcast::Receiver<AgentEvent>,
) -> Vec<AgentEvent> {
    let mut received = Vec::new();
    for _ in 0..200 {
        match events.recv().await {
            Ok(ev) => {
                received.push(ev.clone());
                if matches!(ev, AgentEvent::AgentEnd { .. }) {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    received
}

/// 轮询阻塞式 `get_snapshot` 直到 snapshot.messages 非空（带 deadline）。
async fn wait_snapshot_non_empty(client: &RemoteClient) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let snap = client.get_snapshot().await.expect("get_snapshot");
        if !snap.messages.is_empty() {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("snapshot should have messages");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 轮询阻塞式 `get_snapshot` 直到 snapshot.messages 为空（带 deadline）。
async fn wait_snapshot_empty(client: &RemoteClient) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let snap = client.get_snapshot().await.expect("get_snapshot");
        if snap.messages.is_empty() {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("snapshot should be empty after reset");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 端到端：client prompt → server 驱动 runtime → client 收到完整事件序列与更新后的 snapshot。
#[tokio::test]
async fn test_loopback_end_to_end() {
    let provider = common::FakeProvider::new(vec![common::text_turn("hello there")]);
    let client = connect_with_server(provider).await;

    // prompt 前订阅，确保不丢事件。
    let events = client.subscribe();

    client
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");

    // 读事件直到 AgentEnd。
    let received = read_until_agent_end(events).await;

    // 验证事件骨架（完整有序）。
    let skeleton: Vec<&str> = received
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
        vec!["AgentStart", "TurnStart", "TurnEnd", "AgentEnd"],
        "event sequence should be the complete ordered structural sequence"
    );

    // 验证 snapshot 更新（含 User + Assistant 消息）。
    wait_snapshot_non_empty(&client).await;
    let snap = client.snapshot();
    assert!(
        snap.messages
            .iter()
            .any(|m| matches!(m.as_ref(), Message::User(_))),
        "snapshot should contain a User message"
    );
    assert!(
        snap.messages
            .iter()
            .any(|m| matches!(m.as_ref(), Message::Assistant(_))),
        "snapshot should contain an Assistant message"
    );
}

/// abort 非阻塞：run 进行中（gate 阻塞首次 stream()），abort 立即返回。
///
/// 若 `abort` 等待 run 结束，则会阻塞在 gate（run 未完成、gate 未释放）；
/// 故 `abort` 在 gate 未释放时即返回 `Ok`，证明其非阻塞。
#[tokio::test]
async fn test_abort_non_blocking() {
    // gate 阻塞首次 stream() 调用，使 run 处于进行中。
    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
    let provider = common::FakeProvider::with(vec![common::text_turn("ok")], 0, Some(gate_rx));
    let client = connect_with_server(provider).await;

    // 发 prompt（server 接受命令即返回；run 开始并阻塞在 gate）。
    client
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");

    // abort 非阻塞：gate 未释放（run 未完成），abort 仍立即返回 Ok。
    client.abort().expect("abort should succeed");

    // 释放 gate，让 run 继续（被 abort 取消），避免 runtime task 永久阻塞。
    let _ = gate_tx.send(());
}

/// reset 后 snapshot.messages 清空。
#[tokio::test]
async fn test_reset_clears_messages() {
    let provider = common::FakeProvider::new(vec![common::text_turn("hello")]);
    let client = connect_with_server(provider).await;

    // prompt 后，等待处理完成。
    let events = client.subscribe();
    client
        .prompt(vec![user_msg("hi")])
        .await
        .expect("prompt should succeed");
    read_until_agent_end(events).await;

    // 验证 snapshot 非空。
    wait_snapshot_non_empty(&client).await;
    assert!(!client.snapshot().messages.is_empty());

    // 发 reset。
    client.reset().await.expect("reset should succeed");

    // 等待 snapshot 清空。
    wait_snapshot_empty(&client).await;
    assert!(client.snapshot().messages.is_empty());
}
