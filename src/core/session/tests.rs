//! `session` 模块单元测试：`reduce` 纯函数校验 + `SessionRecorder` 游标语义。
//!
//! 从 `session.rs` 拆出以控制主文件行数（conventions 体量限制）。
//! 全部纯内存（fake storage），无 IO、无网络。

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
