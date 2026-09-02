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
use guigu::server::{AgentServer, ServerError, ServerMessage, ServerRequest};
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
/// 验证事件路由（session/lane 前缀正确），最后断开连接（不关 session/lane）。
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

    // 断开连接（EOF）：只移除该连接的订阅，不关 session/lane。
    drop(reader);
    drop(write_half);
    let _ = server_task.await;

    Ok(())
}

/// 建立 duplex 连接，返回客户端读 / 写侧（服务端 spawn 为 task）。
async fn connect_client(
    server: &Arc<AgentServer>,
) -> (
    LineReader<impl tokio::io::AsyncRead + Unpin>,
    impl tokio::io::AsyncWrite + Unpin,
) {
    let (client_stream, server_stream) = duplex(4096);
    let server2 = Arc::clone(server);
    tokio::spawn(async move {
        let _ = server2.serve_connection(server_stream).await;
    });
    let (read_half, write_half) = tokio::io::split(client_stream);
    (LineReader::new(read_half), write_half)
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

/// 并发重复 spawn_lane：恰好一个成功，另一个 `LaneAlreadyExists`
/// （不覆盖已有 lane、不泄漏 runtime）。
#[tokio::test]
async fn test_concurrent_spawn_lane_race() {
    let provider = common::FakeProvider::new(vec![]);
    let server = AgentServer::new();
    server
        .create_session("s1".to_string(), Arc::new(InMemoryStorage::new()))
        .await
        .expect("create");

    let (r1, r2) = tokio::join!(
        server.spawn_lane(
            "s1",
            "l1",
            common::make_config(),
            common::make_runtime(
                provider.clone(),
                vec![],
                ToolExecutionMode::Sequential,
                8192
            ),
        ),
        server.spawn_lane(
            "s1",
            "l1",
            common::make_config(),
            common::make_runtime(
                provider.clone(),
                vec![],
                ToolExecutionMode::Sequential,
                8192
            ),
        ),
    );

    let results = [r1, r2];
    let ok = results.iter().filter(|r| r.is_ok()).count();
    let dup = results
        .iter()
        .filter(|r| matches!(r, Err(ServerError::LaneAlreadyExists(_))))
        .count();
    assert_eq!(ok, 1, "exactly one spawn should succeed: {results:?}");
    assert_eq!(
        dup, 1,
        "the other should get LaneAlreadyExists: {results:?}"
    );

    // 登记的 lane 可用（未被失败的 spawn 覆盖）。
    assert!(server.snapshot("s1", "l1").await.is_some());
    // shutdown 完成（失败 spawn 的 runtime 已清理，不挂起）。
    server.shutdown().await.expect("shutdown");
}

/// 并发重复 fork_lane：恰好一个成功，另一个 `LaneAlreadyExists`。
#[tokio::test]
async fn test_concurrent_fork_lane_race() {
    let provider = common::FakeProvider::new(vec![]);
    let server = AgentServer::new();
    server
        .create_session("s1".to_string(), Arc::new(InMemoryStorage::new()))
        .await
        .expect("create");
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

    let (r1, r2) = tokio::join!(
        server.fork_lane(
            "s1",
            "l1",
            "l2",
            common::make_config(),
            common::make_runtime(
                provider.clone(),
                vec![],
                ToolExecutionMode::Sequential,
                8192
            ),
        ),
        server.fork_lane(
            "s1",
            "l1",
            "l2",
            common::make_config(),
            common::make_runtime(
                provider.clone(),
                vec![],
                ToolExecutionMode::Sequential,
                8192
            ),
        ),
    );

    let results = [r1, r2];
    let ok = results.iter().filter(|r| r.is_ok()).count();
    let dup = results
        .iter()
        .filter(|r| matches!(r, Err(ServerError::LaneAlreadyExists(_))))
        .count();
    assert_eq!(ok, 1, "exactly one fork should succeed: {results:?}");
    assert_eq!(
        dup, 1,
        "the other should get LaneAlreadyExists: {results:?}"
    );

    assert!(server.snapshot("s1", "l2").await.is_some());
    server.shutdown().await.expect("shutdown");
}

/// spawn_lane 与 shutdown 并发：无「spawn 成功但未登记」的幽灵态——
/// 要么 Ok（已登记，随后被 shutdown 关闭），要么 SessionNotFound（session 被并发关闭）。
#[tokio::test]
async fn test_spawn_lane_vs_shutdown() {
    for _ in 0..20 {
        let provider = common::FakeProvider::new(vec![]);
        let server = AgentServer::new();
        server
            .create_session("s1".to_string(), Arc::new(InMemoryStorage::new()))
            .await
            .expect("create");

        let spawn_task = {
            let server = server.clone();
            let provider = provider.clone();
            tokio::spawn(async move {
                server
                    .spawn_lane(
                        "s1",
                        "l1",
                        common::make_config(),
                        common::make_runtime(provider, vec![], ToolExecutionMode::Sequential, 8192),
                    )
                    .await
            })
        };
        let shutdown_task = {
            let server = server.clone();
            tokio::spawn(async move { server.shutdown().await })
        };

        let spawn_outcome = spawn_task.await.expect("spawn task");
        let shutdown_outcome = shutdown_task.await.expect("shutdown task");
        assert!(
            shutdown_outcome.is_ok(),
            "shutdown should succeed: {shutdown_outcome:?}"
        );
        assert!(
            server.list_sessions().await.is_empty(),
            "registry should be empty after shutdown"
        );
        match spawn_outcome {
            Ok(()) => {}                               // 已登记，随后被 shutdown 关闭
            Err(ServerError::SessionNotFound(_)) => {} // session 被并发关闭
            other => panic!("unexpected spawn outcome: {other:?}"),
        }
    }
}

/// Shutdown 全局语义：一个 client 的 Shutdown 关闭所有 session/lane（不只当前连接）。
#[tokio::test]
async fn test_shutdown_global_semantics() {
    let provider =
        common::FakeProvider::new(vec![common::text_turn("ok1"), common::text_turn("ok2")]);
    let server = Arc::new(make_server(provider));

    // Client A：create s1 + spawn l1。
    let (mut a_reader, mut a_writer) = connect_client(&server).await;
    write_line(
        &mut a_writer,
        &ServerRequest::CreateSession {
            id: 1,
            session_id: Some("s1".to_string()),
        },
    )
    .await
    .expect("write");
    read_response(&mut a_reader, 1).await.expect("create s1");
    write_line(
        &mut a_writer,
        &ServerRequest::SpawnLane {
            id: 2,
            session_id: "s1".to_string(),
            lane_id: "l1".to_string(),
        },
    )
    .await
    .expect("write");
    read_response(&mut a_reader, 2).await.expect("spawn l1");

    // Client B：create s2 + spawn l2。
    let (mut b_reader, mut b_writer) = connect_client(&server).await;
    write_line(
        &mut b_writer,
        &ServerRequest::CreateSession {
            id: 1,
            session_id: Some("s2".to_string()),
        },
    )
    .await
    .expect("write");
    read_response(&mut b_reader, 1).await.expect("create s2");
    write_line(
        &mut b_writer,
        &ServerRequest::SpawnLane {
            id: 2,
            session_id: "s2".to_string(),
            lane_id: "l2".to_string(),
        },
    )
    .await
    .expect("write");
    read_response(&mut b_reader, 2).await.expect("spawn l2");

    assert_eq!(
        server.list_sessions().await,
        vec!["s1".to_string(), "s2".to_string()]
    );

    // Client A 发 Shutdown → Ok 应答，随后连接关闭（EOF）。
    write_line(&mut a_writer, &ServerRequest::Shutdown { id: 3 })
        .await
        .expect("write");
    read_response(&mut a_reader, 3).await.expect("shutdown ok");
    let eof = a_reader.next::<ServerMessage>().await.expect("read");
    assert!(eof.is_none(), "expected EOF after Shutdown");

    // 全局语义：所有 session/lane 关闭（不只 A 的）。
    assert!(
        server.list_sessions().await.is_empty(),
        "Shutdown should close all sessions"
    );

    // Client B 连接仍在，但其 session 已消失：GetSnapshot → Err。
    write_line(
        &mut b_writer,
        &ServerRequest::GetSnapshot {
            id: 3,
            session_id: "s2".to_string(),
            lane_id: "l2".to_string(),
        },
    )
    .await
    .expect("write");
    let msg = b_reader
        .next::<ServerMessage>()
        .await
        .expect("read")
        .expect("some");
    match msg {
        ServerMessage::Response { id, result } => {
            assert_eq!(id, 3);
            assert!(result.is_err(), "expected Err after global shutdown");
        }
        other => panic!("expected Response, got {other:?}"),
    }

    // 清理 client B 连接。
    drop(b_reader);
    drop(b_writer);
}

/// 重复 Subscribe 同一 lane：旧 forwarder 被取消，每个事件只收到一次（不重复）。
#[tokio::test]
async fn test_duplicate_subscribe_no_duplicate_events() {
    let provider = common::FakeProvider::new(vec![common::text_turn("ok")]);
    let server = Arc::new(make_server(provider));

    let (mut reader, mut writer) = connect_client(&server).await;
    write_line(
        &mut writer,
        &ServerRequest::CreateSession {
            id: 1,
            session_id: Some("s1".to_string()),
        },
    )
    .await
    .expect("write");
    read_response(&mut reader, 1).await.expect("create");
    write_line(
        &mut writer,
        &ServerRequest::SpawnLane {
            id: 2,
            session_id: "s1".to_string(),
            lane_id: "l1".to_string(),
        },
    )
    .await
    .expect("write");
    read_response(&mut reader, 2).await.expect("spawn");

    // 订阅两次（重复订阅）。
    write_line(
        &mut writer,
        &ServerRequest::Subscribe {
            id: 3,
            session_id: "s1".to_string(),
            lane_id: "l1".to_string(),
        },
    )
    .await
    .expect("write");
    read_response(&mut reader, 3).await.expect("subscribe 1");
    write_line(
        &mut writer,
        &ServerRequest::Subscribe {
            id: 4,
            session_id: "s1".to_string(),
            lane_id: "l1".to_string(),
        },
    )
    .await
    .expect("write");
    read_response(&mut reader, 4).await.expect("subscribe 2");

    // Prompt。
    write_line(
        &mut writer,
        &ServerRequest::Prompt {
            id: 5,
            session_id: "s1".to_string(),
            lane_id: "l1".to_string(),
            messages: vec![user_msg("hi")],
        },
    )
    .await
    .expect("write");
    read_response(&mut reader, 5).await.expect("prompt");

    // 读事件直到 AgentEnd：AgentStart / AgentEnd 各恰好出现一次
    // （若旧 forwarder 未取消，会各出现两次）。
    let mut agent_start = 0usize;
    let mut agent_end = 0usize;
    for _ in 0..200 {
        let msg = reader
            .next::<ServerMessage>()
            .await
            .expect("read")
            .expect("some");
        if let ServerMessage::Event { event, .. } = msg {
            if matches!(event, AgentEvent::AgentStart) {
                agent_start += 1;
            }
            if matches!(event, AgentEvent::AgentEnd { .. }) {
                agent_end += 1;
                break;
            }
        }
    }
    assert_eq!(agent_start, 1, "AgentStart should be received exactly once");
    assert_eq!(agent_end, 1, "AgentEnd should be received exactly once");
}

/// Unsubscribe 后不再收到该 lane 的事件。
#[tokio::test]
async fn test_unsubscribe_stops_events() {
    let provider = common::FakeProvider::new(vec![common::text_turn("ok")]);
    let server = Arc::new(make_server(provider));

    let (mut reader, mut writer) = connect_client(&server).await;
    write_line(
        &mut writer,
        &ServerRequest::CreateSession {
            id: 1,
            session_id: Some("s1".to_string()),
        },
    )
    .await
    .expect("write");
    read_response(&mut reader, 1).await.expect("create");
    write_line(
        &mut writer,
        &ServerRequest::SpawnLane {
            id: 2,
            session_id: "s1".to_string(),
            lane_id: "l1".to_string(),
        },
    )
    .await
    .expect("write");
    read_response(&mut reader, 2).await.expect("spawn");

    write_line(
        &mut writer,
        &ServerRequest::Subscribe {
            id: 3,
            session_id: "s1".to_string(),
            lane_id: "l1".to_string(),
        },
    )
    .await
    .expect("write");
    read_response(&mut reader, 3).await.expect("subscribe");

    write_line(
        &mut writer,
        &ServerRequest::Unsubscribe {
            id: 4,
            session_id: "s1".to_string(),
            lane_id: "l1".to_string(),
        },
    )
    .await
    .expect("write");
    read_response(&mut reader, 4).await.expect("unsubscribe");

    write_line(
        &mut writer,
        &ServerRequest::Prompt {
            id: 5,
            session_id: "s1".to_string(),
            lane_id: "l1".to_string(),
            messages: vec![user_msg("hi")],
        },
    )
    .await
    .expect("write");
    read_response(&mut reader, 5).await.expect("prompt");

    // Unsubscribe 后不应再收到任何事件（超时 = 无消息）。
    let pending =
        tokio::time::timeout(Duration::from_millis(300), reader.next::<ServerMessage>()).await;
    assert!(
        pending.is_err(),
        "should not receive any message after Unsubscribe"
    );
}
