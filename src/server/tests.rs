//! `AgentServer` 核心 API 单测（Task 013）。
//!
//! 覆盖：create/list/load session；spawn_lane 后 prompt 路由；snapshot/subscribe
//! 返回对应 lane 状态；不存在的 session/lane 返回 `None`/错误不 panic；fork_lane
//! 分支（reduce 出两叶子）。用 `tempfile` 提供真实 JSONL 存储，不依赖外部服务。

use super::*;
use crate::core::agent::AgentConfig;
use crate::core::message::{
    AssistantContent, AssistantMessage, Message, StopReason, ThinkingLevel, UserContent,
    UserMessage,
};
use crate::core::provider::{
    AssistantEvent, AssistantStream, Model, ModelProvider, ProviderError, ProviderRequest,
};
use crate::core::runtime::{AgentRuntime, LoopConfig};
use crate::core::session::{JsonlSessionStorage, SharedSessionStorage};
use async_trait::async_trait;
use futures::stream;
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;

/// 最小 provider：单文本 turn（user + assistant 各一条 `MessageEnd`）。
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

fn user_msg(text: &str) -> Message {
    Message::User(UserMessage {
        content: vec![UserContent::Text {
            text: text.to_string(),
        }],
        timestamp: 0,
    })
}

/// 建一个 `SharedSessionStorage`（包装 `JsonlSessionStorage`，落盘到 `dir/{id}.jsonl`）。
async fn make_storage(dir: &std::path::Path, id: &str) -> Arc<SharedSessionStorage> {
    let jsonl = JsonlSessionStorage::open(dir.join(format!("{id}.jsonl")), id)
        .await
        .expect("open storage");
    Arc::new(SharedSessionStorage::new(Arc::new(jsonl)))
}

/// 轮询 snapshot 直到 messages 非空（带 deadline）。
async fn wait_snapshot_non_empty(server: &AgentServer, session_id: &str, lane_id: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(snap) = server.snapshot(session_id, lane_id).await
            && !snap.messages.is_empty()
        {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("snapshot should have messages");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
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

/// create_session + list_sessions。
#[tokio::test]
async fn test_create_and_list_session() {
    let server = AgentServer::new();
    let dir = tempdir().expect("tempdir");
    let storage = make_storage(dir.path(), "s1").await;
    server
        .create_session("s1".to_string(), storage)
        .await
        .expect("create");

    let sessions = server.list_sessions().await;
    assert_eq!(sessions, vec!["s1".to_string()]);
}

/// 重复 create_session → `DuplicateSession`。
#[tokio::test]
async fn test_duplicate_session() {
    let server = AgentServer::new();
    let dir = tempdir().expect("tempdir");
    let storage = make_storage(dir.path(), "s1").await;
    server
        .create_session("s1".to_string(), storage)
        .await
        .expect("create");

    let storage2 = make_storage(dir.path(), "s1b").await;
    let result = server.create_session("s1".to_string(), storage2).await;
    assert!(matches!(result, Err(ServerError::DuplicateSession(_))));
}

/// load_session：从持久化存储 load + reduce 重建 session（崩溃恢复入口）。
#[tokio::test]
async fn test_load_session() {
    let dir = tempdir().expect("tempdir");
    // 先 create + spawn + prompt，产生持久化数据，再 shutdown。
    let server1 = AgentServer::new();
    let storage = make_storage(dir.path(), "s1").await;
    server1
        .create_session("s1".to_string(), storage.clone())
        .await
        .expect("create");
    server1
        .spawn_lane("s1", "l1", make_config(), make_runtime())
        .await
        .expect("spawn");
    server1
        .prompt("s1", "l1", vec![user_msg("hi")])
        .await
        .expect("prompt");
    wait_tree_nodes(&storage, 2).await;
    server1.shutdown().await.expect("shutdown");

    // 新 server load_session（崩溃恢复）：同一文件，恢复续写游标。
    let server2 = AgentServer::new();
    let storage2 = make_storage(dir.path(), "s1").await;
    server2
        .load_session("s1".to_string(), storage2.clone())
        .await
        .expect("load");

    let sessions = server2.list_sessions().await;
    assert_eq!(sessions, vec!["s1".to_string()]);
    // 持久化数据仍在（load 恢复了游标，树非空）。
    let tree = storage2.load().await.expect("load");
    assert!(!tree.nodes.is_empty());
}

/// spawn_lane 后 prompt 路由正确（snapshot 反映 lane 状态）。
#[tokio::test]
async fn test_spawn_lane_and_prompt() {
    let server = AgentServer::new();
    let dir = tempdir().expect("tempdir");
    let storage = make_storage(dir.path(), "s1").await;
    server
        .create_session("s1".to_string(), storage)
        .await
        .expect("create");
    server
        .spawn_lane("s1", "l1", make_config(), make_runtime())
        .await
        .expect("spawn");

    server
        .prompt("s1", "l1", vec![user_msg("hi")])
        .await
        .expect("prompt");
    wait_snapshot_non_empty(&server, "s1", "l1").await;
}

/// snapshot 返回对应 lane 状态；不存在的 session/lane 返回 `None`（不 panic）。
#[tokio::test]
async fn test_snapshot() {
    let server = AgentServer::new();
    let dir = tempdir().expect("tempdir");
    let storage = make_storage(dir.path(), "s1").await;
    server
        .create_session("s1".to_string(), storage)
        .await
        .expect("create");
    server
        .spawn_lane("s1", "l1", make_config(), make_runtime())
        .await
        .expect("spawn");

    assert!(server.snapshot("s1", "l1").await.is_some());
    assert!(server.snapshot("s1", "l2").await.is_none());
    assert!(server.snapshot("s2", "l1").await.is_none());
}

/// subscribe 返回对应 lane 事件源；不存在的 session/lane 返回 `None`（不 panic）。
#[tokio::test]
async fn test_subscribe() {
    let server = AgentServer::new();
    let dir = tempdir().expect("tempdir");
    let storage = make_storage(dir.path(), "s1").await;
    server
        .create_session("s1".to_string(), storage)
        .await
        .expect("create");
    server
        .spawn_lane("s1", "l1", make_config(), make_runtime())
        .await
        .expect("spawn");

    assert!(server.subscribe("s1", "l1").await.is_some());
    assert!(server.subscribe("s1", "l2").await.is_none());
    assert!(server.subscribe("s2", "l1").await.is_none());
}

/// 不存在的 session → `SessionNotFound`；不存在的 lane → `LaneNotFound`。
#[tokio::test]
async fn test_not_found_errors() {
    let server = AgentServer::new();
    let dir = tempdir().expect("tempdir");
    let storage = make_storage(dir.path(), "s1").await;
    server
        .create_session("s1".to_string(), storage)
        .await
        .expect("create");
    server
        .spawn_lane("s1", "l1", make_config(), make_runtime())
        .await
        .expect("spawn");

    let result = server.prompt("s2", "l1", vec![user_msg("hi")]).await;
    assert!(matches!(result, Err(ServerError::SessionNotFound(_))));

    let result = server.prompt("s1", "l2", vec![user_msg("hi")]).await;
    assert!(matches!(result, Err(ServerError::LaneNotFound(_))));
}

/// 重复 spawn_lane → `LaneAlreadyExists`。
#[tokio::test]
async fn test_lane_already_exists() {
    let server = AgentServer::new();
    let dir = tempdir().expect("tempdir");
    let storage = make_storage(dir.path(), "s1").await;
    server
        .create_session("s1".to_string(), storage)
        .await
        .expect("create");
    server
        .spawn_lane("s1", "l1", make_config(), make_runtime())
        .await
        .expect("spawn");

    let result = server
        .spawn_lane("s1", "l1", make_config(), make_runtime())
        .await;
    assert!(matches!(result, Err(ServerError::LaneAlreadyExists(_))));
}

/// fork_lane：从源 lane 分支后，两 lane 各自续写产生分叉（reduce 出两叶子）。
///
/// 仅 l2 续写只会把链延长（单叶子）；须 l1 在 fork 点后再写一次，使 fork 点
/// 拥有两个子节点（各 lane 一条），`reduce` 才出两叶子。
#[tokio::test]
async fn test_fork_lane_branches() {
    let server = AgentServer::new();
    let dir = tempdir().expect("tempdir");
    let storage = make_storage(dir.path(), "s1").await;
    server
        .create_session("s1".to_string(), storage.clone())
        .await
        .expect("create");

    // spawn 源 lane l1，prompt 产生 user + assistant（持久化 2 节点）。
    server
        .spawn_lane("s1", "l1", make_config(), make_runtime())
        .await
        .expect("spawn");
    server
        .prompt("s1", "l1", vec![user_msg("hi")])
        .await
        .expect("prompt");
    wait_tree_nodes(&storage, 2).await;

    // fork 新 lane l2 从 l1（writer fork_at l1 head）。
    server
        .fork_lane("s1", "l1", "l2", make_config(), make_runtime())
        .await
        .expect("fork");
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

/// fork_lane 错误路径：源 lane 不存在 → `LaneNotFound`；新 lane 已存在 →
/// `LaneAlreadyExists`。
#[tokio::test]
async fn test_fork_lane_errors() {
    let server = AgentServer::new();
    let dir = tempdir().expect("tempdir");
    let storage = make_storage(dir.path(), "s1").await;
    server
        .create_session("s1".to_string(), storage)
        .await
        .expect("create");
    server
        .spawn_lane("s1", "l1", make_config(), make_runtime())
        .await
        .expect("spawn");

    // 源 lane 不存在。
    let result = server
        .fork_lane("s1", "l9", "l2", make_config(), make_runtime())
        .await;
    assert!(matches!(result, Err(ServerError::LaneNotFound(_))));

    // 新 lane 已存在。
    let result = server
        .fork_lane("s1", "l1", "l1", make_config(), make_runtime())
        .await;
    assert!(matches!(result, Err(ServerError::LaneAlreadyExists(_))));
}

/// shutdown 后注册表清空（list_sessions 为空）。
#[tokio::test]
async fn test_shutdown_clears_registry() {
    let server = AgentServer::new();
    let dir = tempdir().expect("tempdir");
    let storage = make_storage(dir.path(), "s1").await;
    server
        .create_session("s1".to_string(), storage)
        .await
        .expect("create");
    server
        .spawn_lane("s1", "l1", make_config(), make_runtime())
        .await
        .expect("spawn");

    server.shutdown().await.expect("shutdown");
    assert!(server.list_sessions().await.is_empty());
}
