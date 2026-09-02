//! Task 013 集成测试：多 client 并发 + fork_lane 分支。
//!
//! - 多连接（duplex loopback）：两 client 并发连同一 `AgentServer`，各自 create
//!   session + spawn lane + prompt，事件各归各（session/lane 前缀路由正确）；
//!   一 client 断开不影响另一 client 的 session。
//! - fork_lane：从源 lane 分支后，新 lane 写落新分支（`reduce` 出两叶子）。
//!
//! 不依赖真实网络（duplex / 内存存储）；异步测试用 `tokio::test`。

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use guigu::core::event::AgentEvent;
use guigu::core::message::{Message, UserContent, UserMessage};
use guigu::core::provider::ModelProvider;
use guigu::core::session::{
    NodeId, SessionEntry, SessionError, SessionStorage, SessionTree, SharedSessionStorage, reduce,
};
use guigu::core::{AgentRuntime, LoopConfig, ToolExecutionMode};
use guigu::remote::codec::{LineReader, write_line};
use guigu::server::{AgentServer, ServerMessage, ServerRequest};
use tokio::io::duplex;

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

fn user_msg(text: &str) -> Message {
    Message::User(UserMessage {
        content: vec![UserContent::Text {
            text: text.to_string(),
        }],
        timestamp: 0,
    })
}

/// 建一个带工厂的 server（storage 用共享内存存储，runtime 用注入的 provider）。
fn make_server(provider: Arc<dyn ModelProvider>) -> AgentServer {
    let server = AgentServer::new();
    server.with_runtime_factory(move || {
        (
            common::make_config(),
            AgentRuntime {
                provider: provider.clone(),
                tools: Vec::new(),
                loop_config: LoopConfig {
                    retry_base_delay: Duration::from_millis(1),
                    ..LoopConfig::default()
                },
            },
        )
    });
    server.with_storage_factory(|_id| {
        Arc::new(SharedSessionStorage::new(Arc::new(InMemoryStorage::new())))
    });
    server
}

/// 读消息直到找到指定 id 的 `Response`（跳过 Event/Snapshot/SessionList）。
async fn read_response<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut LineReader<R>,
    expected_id: u64,
) -> Result<(), String> {
    for _ in 0..200 {
        let msg = reader
            .next::<ServerMessage>()
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "EOF".to_string())?;
        if let ServerMessage::Response { id, result } = msg
            && id == expected_id
        {
            return result.map(|_| ());
        }
    }
    Err("timeout waiting for response".to_string())
}

/// 轮询 storage.load() 直到树有至少 `min_nodes` 个节点（带 deadline）。
async fn wait_tree_nodes(storage: &Arc<SharedSessionStorage>, min_nodes: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let tree = storage.load().await.expect("load");
        if tree.nodes.len() >= min_nodes {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "tree should have at least {min_nodes} nodes, got {}",
                tree.nodes.len()
            );
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 轮询 storage.load() 直到树有至少 `min_leaves` 个叶子（带 deadline）。
async fn wait_tree_leaves(storage: &Arc<SharedSessionStorage>, min_leaves: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let tree = storage.load().await.expect("load");
        if tree.leaves().len() >= min_leaves {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "tree should have at least {min_leaves} leaves, got {:?}",
                tree.leaves()
            );
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 单个 client 的完整流程：create session + spawn lane + subscribe + prompt，
/// 验证事件路由（session/lane 前缀正确），最后 Shutdown 关闭连接。
async fn client_flow(
    server: Arc<AgentServer>,
    session_id: &str,
    lane_id: &str,
    prompt_text: &str,
) -> Result<(), String> {
    let (client_stream, server_stream) = duplex(4096);
    let server2 = Arc::clone(&server);
    let server_task = tokio::spawn(async move {
        let _ = server2.serve_connection(server_stream).await;
    });

    let (read_half, mut write_half) = tokio::io::split(client_stream);
    let mut reader = LineReader::new(read_half);

    // CreateSession。
    write_line(
        &mut write_half,
        &ServerRequest::CreateSession {
            id: 1,
            session_id: Some(session_id.to_string()),
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    read_response(&mut reader, 1).await?;

    // SpawnLane。
    write_line(
        &mut write_half,
        &ServerRequest::SpawnLane {
            id: 2,
            session_id: session_id.to_string(),
            lane_id: lane_id.to_string(),
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    read_response(&mut reader, 2).await?;

    // Subscribe。
    write_line(
        &mut write_half,
        &ServerRequest::Subscribe {
            id: 3,
            session_id: session_id.to_string(),
            lane_id: lane_id.to_string(),
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    read_response(&mut reader, 3).await?;

    // Prompt。
    write_line(
        &mut write_half,
        &ServerRequest::Prompt {
            id: 4,
            session_id: session_id.to_string(),
            lane_id: lane_id.to_string(),
            messages: vec![user_msg(prompt_text)],
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    read_response(&mut reader, 4).await?;

    // 读事件直到 AgentEnd，验证 session/lane 前缀路由正确。
    let mut got_agent_end = false;
    for _ in 0..200 {
        let msg = reader
            .next::<ServerMessage>()
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "EOF".to_string())?;
        match msg {
            ServerMessage::Event {
                session_id: sid,
                lane_id: lid,
                event,
            } => {
                assert_eq!(sid, session_id, "event session prefix mismatch");
                assert_eq!(lid, lane_id, "event lane prefix mismatch");
                if matches!(event, AgentEvent::AgentEnd { .. }) {
                    got_agent_end = true;
                    break;
                }
            }
            ServerMessage::Response { .. }
            | ServerMessage::Snapshot { .. }
            | ServerMessage::SessionList { .. } => {}
        }
    }
    assert!(
        got_agent_end,
        "should receive AgentEnd for {session_id}/{lane_id}"
    );

    // Shutdown 关闭连接。
    write_line(&mut write_half, &ServerRequest::Shutdown { id: 5 })
        .await
        .map_err(|e| e.to_string())?;
    read_response(&mut reader, 5).await?;

    drop(reader);
    drop(write_half);
    let _ = server_task.await;

    Ok(())
}

/// 多 client 并发：两 client 连同一 `AgentServer`，各自 create session + spawn
/// lane + prompt，事件各归各（session/lane 前缀路由正确）。
#[tokio::test]
async fn test_multi_client_concurrency() {
    let provider =
        common::FakeProvider::new(vec![common::text_turn("ok1"), common::text_turn("ok2")]);
    let server = Arc::new(make_server(provider));

    let (result1, result2) = tokio::join!(
        client_flow(server.clone(), "s1", "l1", "hi1"),
        client_flow(server.clone(), "s2", "l2", "hi2"),
    );

    assert!(result1.is_ok(), "client 1 failed: {:?}", result1.err());
    assert!(result2.is_ok(), "client 2 failed: {:?}", result2.err());

    // 两个 session 都注册成功。
    let sessions = server.list_sessions().await;
    assert_eq!(sessions, vec!["s1".to_string(), "s2".to_string()]);
}

/// 一 client 断开不影响另一 client 的 session（多 client 共享 session 语义）。
#[tokio::test]
async fn test_client_disconnect_does_not_affect_other() {
    let provider =
        common::FakeProvider::new(vec![common::text_turn("ok1"), common::text_turn("ok2")]);
    let server = Arc::new(make_server(provider));

    // 两 client 并发完成各自流程（含 Shutdown 断开）。
    let (result1, result2) = tokio::join!(
        client_flow(server.clone(), "s1", "l1", "hi1"),
        client_flow(server.clone(), "s2", "l2", "hi2"),
    );
    assert!(result1.is_ok(), "client 1 failed: {:?}", result1.err());
    assert!(result2.is_ok(), "client 2 failed: {:?}", result2.err());

    // 两 client 均已断开，但 session/lane 仍在 server 注册表（连接关闭不关
    // session/lane）。验证 s2/l2 的 snapshot 仍可查。
    let snap = server.snapshot("s2", "l2").await;
    assert!(
        snap.is_some(),
        "client 2's session should still be alive after disconnect"
    );
    assert!(
        !snap.expect("snapshot").messages.is_empty(),
        "client 2's lane should have messages"
    );
}

/// fork_lane：从源 lane 分支后，两 lane 各自续写产生分叉（`reduce` 出两叶子）。
///
/// 仅 l2 续写只会把链延长（单叶子）；须 l1 在 fork 点后再写一次，使 fork 点
/// 拥有两个子节点（各 lane 一条），`reduce` 才出两叶子。
#[tokio::test]
async fn test_fork_lane_branches() {
    let provider = common::FakeProvider::new(vec![
        common::text_turn("ok1"),
        common::text_turn("ok2"),
        common::text_turn("ok3"),
    ]);
    let server = AgentServer::new();

    // 直接创建共享存储（不通过工厂），以便验证树结构。
    let storage = Arc::new(SharedSessionStorage::new(Arc::new(InMemoryStorage::new())));
    server
        .create_session("s1".to_string(), storage.clone())
        .await
        .expect("create");

    // spawn 源 lane l1，prompt 产生 user + assistant（持久化 2 节点）。
    server
        .spawn_lane(
            "s1",
            "l1",
            common::make_config(),
            common::make_runtime(
                provider.clone(),
                vec![],
                ToolExecutionMode::Sequential,
                8192,
            ),
        )
        .await
        .expect("spawn l1");
    server
        .prompt("s1", "l1", vec![user_msg("hi")])
        .await
        .expect("prompt l1");
    wait_tree_nodes(&storage, 2).await;

    // fork 新 lane l2 从 l1（writer fork_at l1 head）。
    server
        .fork_lane(
            "s1",
            "l1",
            "l2",
            common::make_config(),
            common::make_runtime(
                provider.clone(),
                vec![],
                ToolExecutionMode::Sequential,
                8192,
            ),
        )
        .await
        .expect("fork l2");
    // l2 写新分支（parent = l1 head）。
    server
        .prompt("s1", "l2", vec![user_msg("branch")])
        .await
        .expect("prompt l2");
    // l1 在 fork 点后再写一次：与 l2 分叉，reduce 出两叶子。
    server
        .prompt("s1", "l1", vec![user_msg("again")])
        .await
        .expect("prompt l1 again");
    wait_tree_leaves(&storage, 2).await;
}
