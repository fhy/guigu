//! Task 009 集成测试：`JsonlSessionStorage` tempdir 驱动（脱离网络、无外部服务）。
//!
//! 覆盖：线性 append → load；fork 两分支；崩溃恢复（尾部半行）；文件不存在 → 空树；
//! 崩溃恢复后续写不重复 id；`load` 恢复续写游标；`SessionRecorder::attach` 端到端落盘。

use std::sync::Arc;

use guigu::core::event::AgentEvent;
use guigu::core::message::{Message, UserContent, UserMessage};
use guigu::core::session::{JsonlSessionStorage, SessionEntry, SessionRecorder, SessionStorage};
use tokio::io::AsyncWriteExt;
use tokio::sync::broadcast;

/// 构造 User 文本消息（测试用）。
fn user_msg(text: &str) -> Message {
    Message::User(UserMessage {
        content: vec![UserContent::Text {
            text: text.to_string(),
        }],
        timestamp: 0,
    })
}

/// 序列化 entry 为 JSONL 一行（含行尾换行，测试用）。
fn line(id: u64, parent: Option<u64>, text: &str) -> String {
    let entry = SessionEntry {
        id,
        parent_id: parent,
        message: user_msg(text),
    };
    format!("{}\n", serde_json::to_string(&entry).unwrap())
}

#[tokio::test]
async fn linear_append_then_load() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let storage = JsonlSessionStorage::open(&path, "s1").await.unwrap();
    assert_eq!(storage.next_id(), 0);
    let i0 = storage.append(None, user_msg("a")).await.unwrap();
    let i1 = storage.append(Some(i0), user_msg("b")).await.unwrap();
    let i2 = storage.append(Some(i1), user_msg("c")).await.unwrap();
    assert_eq!((i0, i1, i2), (0, 1, 2));
    assert_eq!(storage.next_id(), 3);

    let tree = storage.load().await.unwrap();
    assert_eq!(tree.session_id, "s1");
    assert_eq!(tree.root, Some(0));
    assert_eq!(tree.nodes.len(), 3);
    assert_eq!(tree.leaves(), vec![2]);
    assert_eq!(tree.path_to(2).unwrap().len(), 3);
}

#[tokio::test]
async fn fork_creates_two_branches() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let storage = JsonlSessionStorage::open(&path, "s1").await.unwrap();
    let i0 = storage.append(None, user_msg("a")).await.unwrap();
    let i1 = storage.append(Some(i0), user_msg("b")).await.unwrap();
    // fork：向历史节点 i0 追加
    let i2 = storage.append(Some(i0), user_msg("c")).await.unwrap();
    assert_eq!((i0, i1, i2), (0, 1, 2));

    let tree = storage.load().await.unwrap();
    assert_eq!(tree.nodes.len(), 3);
    assert_eq!(tree.leaves(), vec![1, 2]);
    // 两分支根到叶序列：共享根、末条不同
    let branch_b = tree.path_to(1).unwrap();
    let branch_c = tree.path_to(2).unwrap();
    assert_eq!(branch_b.len(), 2);
    assert_eq!(branch_c.len(), 2);
    assert_eq!(branch_b[0], branch_c[0]);
    assert_ne!(branch_b[1], branch_c[1]);
}

#[tokio::test]
async fn crash_recovery_ignores_trailing_half_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    // 模拟崩溃：2 条完整行 + 末尾半行（截断、无行尾换行）。
    let half: String = line(2, Some(1), "c").chars().take(20).collect();
    tokio::fs::write(
        &path,
        format!("{}{}{}", line(0, None, "a"), line(1, Some(0), "b"), half),
    )
    .await
    .unwrap();

    let storage = JsonlSessionStorage::open(&path, "s1").await.unwrap();
    assert_eq!(storage.next_id(), 2); // max(id)=1 → 2
    let tree = storage.load().await.unwrap();
    assert_eq!(tree.nodes.len(), 2); // 半行被忽略
    assert_eq!(tree.root, Some(0));
    assert_eq!(tree.leaves(), vec![1]);
}

#[tokio::test]
async fn open_missing_file_yields_empty_tree() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nope.jsonl");
    let storage = JsonlSessionStorage::open(&path, "s1").await.unwrap();
    assert_eq!(storage.next_id(), 0);
    let tree = storage.load().await.unwrap();
    assert_eq!(tree.root, None);
    assert!(tree.nodes.is_empty());
    assert!(path.exists()); // open 已创建文件
}

#[tokio::test]
async fn append_after_recovery_does_not_duplicate_ids() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    // 模拟崩溃残留 3 条完整行（id 0..2）。
    tokio::fs::write(
        &path,
        format!(
            "{}{}{}",
            line(0, None, "a"),
            line(1, Some(0), "b"),
            line(2, Some(1), "c")
        ),
    )
    .await
    .unwrap();

    let storage = JsonlSessionStorage::open(&path, "s1").await.unwrap();
    assert_eq!(storage.next_id(), 3);
    let i3 = storage.append(Some(2), user_msg("d")).await.unwrap();
    assert_eq!(i3, 3); // 续写不重复

    let tree = storage.load().await.unwrap();
    assert_eq!(tree.nodes.len(), 4);
    assert_eq!(tree.leaves(), vec![3]);
    let ids: Vec<u64> = tree.nodes.keys().copied().collect();
    assert_eq!(ids, vec![0, 1, 2, 3]);
}

#[tokio::test]
async fn load_restores_next_id_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let storage = JsonlSessionStorage::open(&path, "s1").await.unwrap();
    storage.append(None, user_msg("a")).await.unwrap(); // 0
    storage.append(Some(0), user_msg("b")).await.unwrap(); // 1
    // 模拟崩溃进程留下更高游标：手工追加 id 2、3 两行。
    let mut f = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .await
        .unwrap();
    f.write_all(format!("{}{}", line(2, Some(1), "c"), line(3, Some(2), "d")).as_bytes())
        .await
        .unwrap();

    let tree = storage.load().await.unwrap();
    assert_eq!(tree.nodes.len(), 4);
    assert_eq!(storage.next_id(), 4); // load 恢复游标
    let i4 = storage.append(Some(3), user_msg("e")).await.unwrap();
    assert_eq!(i4, 4);
}

#[tokio::test]
async fn recorder_attach_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let storage = Arc::new(JsonlSessionStorage::open(&path, "s1").await.unwrap());
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
    tx.send(AgentEvent::MessageEnd {
        message: Arc::new(user_msg("world")),
    })
    .unwrap();
    drop(tx);
    handle.await.unwrap();

    let tree = storage.load().await.unwrap();
    assert_eq!(tree.nodes.len(), 2);
    assert_eq!(tree.root, Some(0));
    let leaf = tree.leaves().pop().unwrap();
    assert_eq!(tree.path_to(leaf).unwrap().len(), 2);
}
