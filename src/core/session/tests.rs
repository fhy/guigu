//! `session` 模块单元测试：`reduce` 纯函数校验 + `SessionRecorder` 游标语义。
//!
//! 从 `session.rs` 拆出以控制主文件行数（conventions 体量限制）。
//! 全部纯内存（fake storage），无 IO、无网络。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use tokio::sync::{Mutex, broadcast};

use super::*;
use crate::core::message::{UserContent, UserMessage};

/// 构造 User 文本消息（测试用）。
fn user_msg(text: &str) -> Message {
    Message::User(UserMessage {
        content: vec![UserContent::Text {
            text: text.to_string(),
        }],
        timestamp: 0,
    })
}

/// 构造 entry（测试用）。
fn entry(id: NodeId, parent: Option<NodeId>, text: &str) -> SessionEntry {
    SessionEntry {
        id,
        parent_id: parent,
        message: user_msg(text),
    }
}

/// 内存版 `SessionStorage`：记录 entries，用于验证 recorder 游标语义（无 IO）。
#[derive(Default)]
struct MemStorage {
    entries: Mutex<Vec<SessionEntry>>,
    next_id: AtomicU64,
}

#[async_trait]
impl SessionStorage for MemStorage {
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

#[test]
fn reduce_linear_chain() {
    let tree = reduce(vec![
        entry(0, None, "a"),
        entry(1, Some(0), "b"),
        entry(2, Some(1), "c"),
    ])
    .unwrap();
    assert_eq!(tree.root, Some(0));
    assert_eq!(tree.nodes.len(), 3);
    assert_eq!(tree.nodes[&0].children, vec![1]);
    assert_eq!(tree.nodes[&1].children, vec![2]);
    assert_eq!(tree.leaves(), vec![2]);
}

#[test]
fn reduce_fork_yields_two_leaves() {
    let tree = reduce(vec![
        entry(0, None, "a"),
        entry(1, Some(0), "b"),
        entry(2, Some(0), "c"), // fork：与 1 同 parent
    ])
    .unwrap();
    assert_eq!(tree.root, Some(0));
    assert_eq!(tree.nodes[&0].children, vec![1, 2]);
    assert_eq!(tree.leaves(), vec![1, 2]);
}

#[test]
fn reduce_path_to_returns_root_to_leaf_sequence() {
    let tree = reduce(vec![
        entry(0, None, "a"),
        entry(1, Some(0), "b"),
        entry(2, Some(1), "c"),
    ])
    .unwrap();
    let path = tree.path_to(2).unwrap();
    assert_eq!(path, vec![&user_msg("a"), &user_msg("b"), &user_msg("c")]);
    assert_eq!(tree.path_to(99), None);
}

#[test]
fn reduce_path_to_internal_node_returns_none() {
    // path_to 仅定义于叶：内部节点（children 非空）返回 None，
    // 调用方据此区分完整 transcript 与中间路径。
    let tree = reduce(vec![
        entry(0, None, "a"),
        entry(1, Some(0), "b"),
        entry(2, Some(1), "c"),
    ])
    .unwrap();
    assert_eq!(tree.path_to(0), None); // 根（有 child）
    assert_eq!(tree.path_to(1), None); // 中间节点
    assert!(tree.path_to(2).is_some()); // 叶
}

#[test]
fn reduce_path_to_single_node_root_is_leaf() {
    // 单节点树：根即叶（children 为空），path_to 返回单条序列。
    let tree = reduce(vec![entry(0, None, "a")]).unwrap();
    let path = tree.path_to(0).unwrap();
    assert_eq!(path, vec![&user_msg("a")]);
}

#[test]
fn reduce_duplicate_id() {
    let err = reduce(vec![entry(0, None, "a"), entry(0, None, "b")]).unwrap_err();
    assert!(matches!(err, SessionError::DuplicateNode(0)));
}

#[test]
fn reduce_parent_not_found() {
    let err = reduce(vec![entry(0, None, "a"), entry(1, Some(7), "b")]).unwrap_err();
    assert!(matches!(err, SessionError::ParentNotFound(7)));
}

#[test]
fn reduce_multiple_roots() {
    let err = reduce(vec![entry(0, None, "a"), entry(1, None, "b")]).unwrap_err();
    assert!(matches!(err, SessionError::MultipleRoots));
}

#[test]
fn reduce_cycle() {
    let err = reduce(vec![entry(1, Some(2), "a"), entry(2, Some(1), "b")]).unwrap_err();
    assert!(matches!(err, SessionError::Cycle));
}

#[test]
fn reduce_empty() {
    let tree = reduce(Vec::new()).unwrap();
    assert_eq!(tree.root, None);
    assert!(tree.nodes.is_empty());
    assert!(tree.leaves().is_empty());
}

#[test]
fn reduce_children_filled_in_ascending_id_order() {
    // entry 顺序非 id 顺序，children 仍须按 id 升序填充。
    let tree = reduce(vec![
        entry(0, None, "root"),
        entry(5, Some(0), "e5"),
        entry(3, Some(0), "e3"),
        entry(7, Some(0), "e7"),
    ])
    .unwrap();
    assert_eq!(tree.nodes[&0].children, vec![3, 5, 7]);
}

#[test]
fn reduce_parent_after_child_is_accepted() {
    // parent 存在性为全局校验，不依赖 entry 顺序（reduce 是公开纯函数）。
    let tree = reduce(vec![entry(1, Some(2), "child"), entry(2, None, "parent")]).unwrap();
    assert_eq!(tree.root, Some(2));
    assert_eq!(tree.nodes[&2].children, vec![1]);
}

#[tokio::test]
async fn recorder_record_advances_head() {
    let storage = Arc::new(MemStorage::default());
    let mut rec = SessionRecorder::new(storage.clone());
    let id0 = rec.record(user_msg("a")).await.unwrap();
    let id1 = rec.record(user_msg("b")).await.unwrap();
    assert_eq!((id0, id1), (0, 1));
    let entries = storage.entries.lock().await.clone();
    assert_eq!(entries[0].parent_id, None);
    assert_eq!(entries[1].parent_id, Some(0));
}

#[tokio::test]
async fn recorder_fork_at_redirects_head() {
    let storage = Arc::new(MemStorage::default());
    let mut rec = SessionRecorder::new(storage.clone());
    rec.record(user_msg("a")).await.unwrap(); // 0
    rec.record(user_msg("b")).await.unwrap(); // 1
    rec.fork_at(0);
    let id = rec.record(user_msg("c")).await.unwrap(); // 2，parent = 0
    assert_eq!(id, 2);
    let entries = storage.entries.lock().await.clone();
    assert_eq!(entries[2].parent_id, Some(0));
    let tree = storage.load().await.unwrap();
    assert_eq!(tree.leaves(), vec![1, 2]);
}

#[tokio::test]
async fn recorder_attach_consumes_message_end_only() {
    let storage = Arc::new(MemStorage::default());
    let (tx, rx) = broadcast::channel(8);
    let mut rec = SessionRecorder::new(storage.clone());
    let handle = tokio::spawn(async move {
        rec.attach(rx).await;
    });
    tx.send(AgentEvent::AgentStart).unwrap();
    tx.send(AgentEvent::MessageEnd {
        message: Arc::new(user_msg("hello")),
    })
    .unwrap();
    tx.send(AgentEvent::TurnStart).unwrap();
    tx.send(AgentEvent::MessageEnd {
        message: Arc::new(user_msg("world")),
    })
    .unwrap();
    drop(tx);
    handle.await.unwrap();
    let entries = storage.entries.lock().await.clone();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].parent_id, None);
    assert_eq!(entries[1].parent_id, Some(0));
}

// ===== Task 012：SharedSessionStorage + LaneWriter =====

#[tokio::test]
async fn shared_storage_delegates_append_load_next_id() {
    let inner = Arc::new(MemStorage::default());
    let shared = Arc::new(SharedSessionStorage::new(inner.clone()));
    assert_eq!(shared.next_id(), 0);
    let id0 = shared.append(None, user_msg("a")).await.unwrap();
    let id1 = shared.append(Some(id0), user_msg("b")).await.unwrap();
    assert_eq!((id0, id1), (0, 1));
    assert_eq!(shared.next_id(), 2);
    let tree = shared.load().await.unwrap();
    assert_eq!(tree.nodes.len(), 2);
    assert_eq!(tree.leaves(), vec![1]);
}

#[tokio::test]
async fn shared_storage_concurrent_appends_unique_ids() {
    let inner = Arc::new(MemStorage::default());
    let shared = Arc::new(SharedSessionStorage::new(inner.clone()));
    // 星型拓扑：先建根，再并发挂子（全 parent=None 会触发 MultipleRoots）。
    let root = shared.append(None, user_msg("root")).await.unwrap();
    let n = 16;
    let mut handles = Vec::new();
    for _ in 0..n {
        let shared = shared.clone();
        handles.push(tokio::spawn(async move {
            shared.append(Some(root), user_msg("x")).await
        }));
    }
    let results = futures::future::join_all(handles).await;
    let ids: Vec<u64> = results.into_iter().map(|r| r.unwrap().unwrap()).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), n, "并发 append 产生重复 id");
    assert_eq!(shared.next_id(), n as NodeId + 1);
    let tree = shared.load().await.unwrap();
    assert_eq!(tree.nodes.len(), n + 1);
    assert_eq!(tree.nodes[&root].children.len(), n);
}

#[tokio::test]
async fn lane_writer_append_advances_head() {
    let storage = Arc::new(SharedSessionStorage::new(Arc::new(MemStorage::default())));
    let mut lane = LaneWriter::new(storage.clone(), "lane-1", None);
    assert_eq!(lane.lane_id(), "lane-1");
    assert_eq!(lane.head(), None);
    let id0 = lane.append(user_msg("a")).await.unwrap();
    let id1 = lane.append(user_msg("b")).await.unwrap();
    let id2 = lane.append(user_msg("c")).await.unwrap();
    assert_eq!((id0, id1, id2), (0, 1, 2));
    assert_eq!(lane.head(), Some(2));
    let tree = storage.load().await.unwrap();
    assert_eq!(tree.leaves(), vec![2]);
    assert_eq!(tree.path_to(2).unwrap().len(), 3);
}

#[tokio::test]
async fn lane_writer_fork_at_redirects_head() {
    let storage = Arc::new(SharedSessionStorage::new(Arc::new(MemStorage::default())));
    let mut lane = LaneWriter::new(storage.clone(), "lane-1", None);
    lane.append(user_msg("a")).await.unwrap(); // 0
    lane.append(user_msg("b")).await.unwrap(); // 1
    lane.fork_at(Some(0));
    let id = lane.append(user_msg("c")).await.unwrap(); // 2，parent = 0
    assert_eq!(id, 2);
    assert_eq!(lane.head(), Some(2));
    let tree = storage.load().await.unwrap();
    assert_eq!(tree.leaves(), vec![1, 2]);
}

#[tokio::test]
async fn lane_writer_two_lanes_same_head_two_leaves() {
    let storage = Arc::new(SharedSessionStorage::new(Arc::new(MemStorage::default())));
    let root = storage.append(None, user_msg("root")).await.unwrap(); // 0
    let mut lane_a = LaneWriter::new(storage.clone(), "a", Some(root));
    let mut lane_b = LaneWriter::new(storage.clone(), "b", Some(root));
    let id_a = lane_a.append(user_msg("a")).await.unwrap();
    let id_b = lane_b.append(user_msg("b")).await.unwrap();
    assert_ne!(id_a, id_b);
    let tree = storage.load().await.unwrap();
    assert_eq!(tree.nodes.len(), 3);
    let leaves = tree.leaves();
    assert_eq!(leaves.len(), 2);
    assert!(leaves.contains(&id_a));
    assert!(leaves.contains(&id_b));
    assert_eq!(tree.nodes[&root].children.len(), 2);
}

// ===== Task 024：LaneHeadRecord / SessionRecord / LaneHeadStore / LaneWriter head 持久化 =====

/// 内存版 `LaneHeadStore`：记录 lane_id → head（测试用，无 IO）。
#[derive(Default)]
struct MemHeadStore {
    heads: Mutex<HashMap<LaneId, Option<NodeId>>>,
}

#[async_trait]
impl LaneHeadStore for MemHeadStore {
    async fn append_lane_head(
        &self,
        lane_id: LaneId,
        head: Option<NodeId>,
    ) -> Result<(), SessionError> {
        self.heads.lock().await.insert(lane_id, head);
        Ok(())
    }

    async fn load_lane_heads(&self) -> Result<HashMap<LaneId, Option<NodeId>>, SessionError> {
        Ok(self.heads.lock().await.clone())
    }
}

#[test]
fn session_record_lane_head_roundtrip() {
    let record = SessionRecord::LaneHead(LaneHeadRecord {
        lane_id: "lane-1".to_string(),
        head: Some(5),
    });
    let json = serde_json::to_string(&record).unwrap();
    // LaneHead 输出 {lane_id, head}（无 tag、无 id/parent_id/message）。
    assert!(json.contains("\"lane_id\""));
    assert!(json.contains("\"head\""));
    assert!(!json.contains("\"parent_id\""));
    assert!(!json.contains("\"message\""));
    let parsed: SessionRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, record);
}

#[test]
fn session_record_message_roundtrip() {
    let record = SessionRecord::Message(entry(0, None, "a"));
    let json = serde_json::to_string(&record).unwrap();
    // Message 输出 009 裸形状（无 tag、与旧文件一致）。
    assert!(json.contains("\"id\""));
    assert!(json.contains("\"parent_id\""));
    assert!(!json.contains("\"lane_id\""));
    let parsed: SessionRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, record);
}

#[test]
fn session_record_field_mutual_exclusion() {
    // Message 行不误判为 LaneHead（缺 lane_id → 只能落 Message 变体）。
    let msg_json = serde_json::to_string(&SessionRecord::Message(entry(0, None, "a"))).unwrap();
    let parsed: SessionRecord = serde_json::from_str(&msg_json).unwrap();
    assert!(matches!(parsed, SessionRecord::Message(_)));
    // LaneHead 行不误判为 Message（缺 id/message → 只能落 LaneHead 变体）。
    let head_json = serde_json::to_string(&SessionRecord::LaneHead(LaneHeadRecord {
        lane_id: "l".to_string(),
        head: None,
    }))
    .unwrap();
    let parsed: SessionRecord = serde_json::from_str(&head_json).unwrap();
    assert!(matches!(parsed, SessionRecord::LaneHead(_)));
}

#[tokio::test]
async fn lane_writer_with_head_store_append_auto_persists() {
    // head 持久化由 `SharedSessionStorage` 的 `head_store` 统一决定（024）：
    // 绑定时 `LaneWriter::append` 经 `append_with_head` 原子落盘 head。
    let inner = Arc::new(MemStorage::default());
    let head_store = Arc::new(MemHeadStore::default());
    let storage = Arc::new(SharedSessionStorage::with_head_store(
        inner,
        head_store.clone(),
    ));
    let mut lane = LaneWriter::new(storage.clone(), "lane-1", None);
    let id0 = lane.append(user_msg("a")).await.unwrap();
    let id1 = lane.append(user_msg("b")).await.unwrap();
    assert_eq!((id0, id1), (0, 1));
    // append 后自动落盘 head（后写覆盖先写，最终值 = 最新 head）。
    let heads = head_store.load_lane_heads().await.unwrap();
    assert_eq!(heads.get("lane-1"), Some(&Some(1)));
}

#[tokio::test]
async fn lane_writer_persist_head_explicit_after_fork() {
    let inner = Arc::new(MemStorage::default());
    let head_store = Arc::new(MemHeadStore::default());
    let storage = Arc::new(SharedSessionStorage::with_head_store(
        inner,
        head_store.clone(),
    ));
    let mut lane = LaneWriter::new(storage.clone(), "lane-1", None);
    lane.append(user_msg("a")).await.unwrap(); // 0（自动落盘 head=Some(0)）
    lane.fork_at(Some(0)); // 纯内存，不落盘
    lane.persist_head().await.unwrap(); // 显式落盘 head = Some(0)
    let heads = head_store.load_lane_heads().await.unwrap();
    assert_eq!(heads.get("lane-1"), Some(&Some(0)));
}

#[tokio::test]
async fn lane_writer_new_no_store_persist_head_is_noop() {
    let storage = Arc::new(SharedSessionStorage::new(Arc::new(MemStorage::default())));
    let mut lane = LaneWriter::new(storage.clone(), "lane-1", None);
    let id = lane.append(user_msg("a")).await.unwrap(); // 正常 append
    assert_eq!(id, 0);
    // 无 store：persist_head 空操作、不报错（行为等价 012）。
    lane.persist_head().await.unwrap();
}

#[tokio::test]
async fn shared_storage_lane_head_store_delegates_when_bound() {
    let inner = Arc::new(MemStorage::default());
    let head_store = Arc::new(MemHeadStore::default());
    let shared = Arc::new(SharedSessionStorage::with_head_store(
        inner,
        head_store.clone(),
    ));
    shared
        .append_lane_head("lane-1".to_string(), Some(7))
        .await
        .unwrap();
    let heads = shared.load_lane_heads().await.unwrap();
    assert_eq!(heads.get("lane-1"), Some(&Some(7)));
}

#[tokio::test]
async fn shared_storage_lane_head_store_noop_when_unbound() {
    let shared = Arc::new(SharedSessionStorage::new(Arc::new(MemStorage::default())));
    // 未绑定：append_lane_head 空操作、load_lane_heads 空表（行为等价 012）。
    shared
        .append_lane_head("lane-1".to_string(), Some(7))
        .await
        .unwrap();
    let heads = shared.load_lane_heads().await.unwrap();
    assert!(heads.is_empty());
}

// ===== Task 024 r2：append_with_head 原子提交 + head 写失败 + snapshot 一致性 =====

/// 失败版 `LaneHeadStore`：`append_lane_head` 始终返回 IO 错误（测试用）。
struct FailingHeadStore;

#[async_trait]
impl LaneHeadStore for FailingHeadStore {
    async fn append_lane_head(
        &self,
        _lane_id: LaneId,
        _head: Option<NodeId>,
    ) -> Result<(), SessionError> {
        Err(SessionError::Io(std::io::Error::other(
            "simulated head write failure",
        )))
    }

    async fn load_lane_heads(&self) -> Result<HashMap<LaneId, Option<NodeId>>, SessionError> {
        Ok(HashMap::new())
    }
}

/// head 写失败时 `append_with_head` 整体返回错误（message 可能已落盘，但 head 未落盘）。
#[tokio::test]
async fn append_with_head_fails_when_head_store_fails() {
    let inner = Arc::new(MemStorage::default());
    let shared = Arc::new(SharedSessionStorage::with_head_store(
        inner,
        Arc::new(FailingHeadStore),
    ));
    let err = shared
        .append_with_head("lane-1", None, user_msg("a"))
        .await
        .unwrap_err();
    assert!(matches!(err, SessionError::Io(_)));
}

/// `LaneWriter::append` 绑定 head 持久化时，head 写失败 → append 返回错误、
/// **内存 head 不推进**（杜绝内存与磁盘 head 分裂，024 修复）。
#[tokio::test]
async fn lane_writer_append_head_failure_does_not_advance_head() {
    let inner = Arc::new(MemStorage::default());
    let shared = Arc::new(SharedSessionStorage::with_head_store(
        inner.clone(),
        Arc::new(FailingHeadStore),
    ));
    let mut lane = LaneWriter::new(shared.clone(), "lane-1", None);
    // 首次 append：head 写失败 → 返回错误，内存 head 保持 None。
    let err = lane.append(user_msg("a")).await.unwrap_err();
    assert!(matches!(err, SessionError::Io(_)));
    assert_eq!(lane.head(), None, "head 写失败后内存 head 不得推进");
    // 后续 append 仍从旧 head（None）起步，不基于已分裂的内存 head。
    let err2 = lane.append(user_msg("b")).await.unwrap_err();
    assert!(matches!(err2, SessionError::Io(_)));
    assert_eq!(lane.head(), None);
}

/// `append_with_head` 成功时 message 与 head 原子提交：head 表反映最新 head。
#[tokio::test]
async fn append_with_head_atomic_commit_updates_head() {
    let inner = Arc::new(MemStorage::default());
    let head_store = Arc::new(MemHeadStore::default());
    let shared = Arc::new(SharedSessionStorage::with_head_store(
        inner,
        head_store.clone(),
    ));
    let id0 = shared
        .append_with_head("lane-1", None, user_msg("a"))
        .await
        .unwrap();
    let id1 = shared
        .append_with_head("lane-1", Some(id0), user_msg("b"))
        .await
        .unwrap();
    // head 表反映最新 head（后写覆盖先写）。
    let heads = head_store.load_lane_heads().await.unwrap();
    assert_eq!(heads.get("lane-1"), Some(&Some(id1)));
}

/// `snapshot` 一致性：持读锁一次得 tree + heads，与 append 的写锁互斥。
#[tokio::test]
async fn snapshot_returns_consistent_tree_and_heads() {
    let inner = Arc::new(MemStorage::default());
    let head_store = Arc::new(MemHeadStore::default());
    let shared = Arc::new(SharedSessionStorage::with_head_store(
        inner,
        head_store.clone(),
    ));
    let id0 = shared
        .append_with_head("lane-1", None, user_msg("a"))
        .await
        .unwrap();
    // snapshot 得 tree（1 节点）+ heads（lane-1 → id0）。
    let (tree, heads) = shared.snapshot().await.unwrap();
    assert_eq!(tree.nodes.len(), 1);
    assert_eq!(heads.get("lane-1"), Some(&Some(id0)));
}

/// `snapshot` 未绑定 head store 时 heads 为空表（行为等价 012）。
#[tokio::test]
async fn snapshot_unbound_head_store_returns_empty_heads() {
    let shared = Arc::new(SharedSessionStorage::new(Arc::new(MemStorage::default())));
    shared.append(None, user_msg("a")).await.unwrap();
    let (tree, heads) = shared.snapshot().await.unwrap();
    assert_eq!(tree.nodes.len(), 1);
    assert!(heads.is_empty());
}
