//! Task 009 集成测试：`JsonlSessionStorage` tempdir 驱动（脱离网络、无外部服务）。
//!
//! 覆盖：线性 append → load；fork 两分支；崩溃恢复（尾部半行）；文件不存在 → 空树；
//! 崩溃恢复后续写不重复 id；`load` 恢复续写游标；`SessionRecorder::attach` 端到端落盘。

use std::sync::Arc;

use guigu::core::event::AgentEvent;
use guigu::core::message::{Message, UserContent, UserMessage};
use guigu::core::session::{
    JsonlSessionStorage, LaneWriter, SessionEntry, SessionError, SessionRecorder, SessionStorage,
    SharedSessionStorage,
};
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
async fn open_with_max_id_returns_id_exhausted() {
    // 外部注入 id = u64::MAX 的行：open 恢复游标时 max+1 溢出 → IdExhausted（不回绕为 0）。
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    tokio::fs::write(&path, line(u64::MAX, None, "a"))
        .await
        .unwrap();
    let err = match JsonlSessionStorage::open(&path, "s1").await {
        Err(e) => e,
        Ok(_) => panic!("expected open to fail with IdExhausted"),
    };
    assert!(matches!(err, SessionError::IdExhausted));
}

#[tokio::test]
async fn load_with_max_id_returns_id_exhausted() {
    // open 时 max = u64::MAX - 1（成功，游标 = u64::MAX）；
    // 外部再追加 id = u64::MAX 的行后 load → max+1 溢出 → IdExhausted。
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    tokio::fs::write(&path, line(u64::MAX - 1, None, "a"))
        .await
        .unwrap();
    let storage = JsonlSessionStorage::open(&path, "s1").await.unwrap();
    assert_eq!(storage.next_id(), u64::MAX);
    let mut f = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .await
        .unwrap();
    f.write_all(line(u64::MAX, Some(u64::MAX - 1), "b").as_bytes())
        .await
        .unwrap();
    let err = storage.load().await.unwrap_err();
    assert!(matches!(err, SessionError::IdExhausted));
}

#[tokio::test]
async fn append_with_exhausted_cursor_returns_id_exhausted() {
    // 游标停在 u64::MAX：append 返回 IdExhausted 且游标不回绕（仍为 u64::MAX）。
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    tokio::fs::write(&path, line(u64::MAX - 1, None, "a"))
        .await
        .unwrap();
    let storage = JsonlSessionStorage::open(&path, "s1").await.unwrap();
    assert_eq!(storage.next_id(), u64::MAX);
    let err = storage.append(None, user_msg("b")).await.unwrap_err();
    assert!(matches!(err, SessionError::IdExhausted));
    assert_eq!(storage.next_id(), u64::MAX); // 未回绕
}

#[tokio::test]
async fn append_allocates_up_to_max_minus_one_then_exhausts() {
    // 验证「最后可分配 id = u64::MAX - 1」的边界语义：
    // 游标到 u64::MAX 后 append 耗尽（IdExhausted），且不再分配 id = u64::MAX。
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    tokio::fs::write(&path, line(u64::MAX - 2, None, "a"))
        .await
        .unwrap();
    let storage = JsonlSessionStorage::open(&path, "s1").await.unwrap();
    assert_eq!(storage.next_id(), u64::MAX - 1);
    // 分配最后一个合法 id = u64::MAX - 1
    let last = storage
        .append(Some(u64::MAX - 2), user_msg("b"))
        .await
        .unwrap();
    assert_eq!(last, u64::MAX - 1);
    assert_eq!(storage.next_id(), u64::MAX);
    // 再 append → 耗尽
    let err = storage.append(Some(last), user_msg("c")).await.unwrap_err();
    assert!(matches!(err, SessionError::IdExhausted));
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

// ===== Task 012：SharedSessionStorage + LaneWriter（JsonlSessionStorage 组合）=====

#[tokio::test]
async fn shared_storage_concurrent_appends_no_interleave() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let inner = Arc::new(JsonlSessionStorage::open(&path, "s1").await.unwrap());
    let shared = Arc::new(SharedSessionStorage::new(inner));

    // 星型拓扑：先建根，再并发挂子（全 parent=None 会触发 MultipleRoots）。
    let root = shared.append(None, user_msg("root")).await.unwrap();
    let n = 32;
    let mut handles = Vec::new();
    for i in 0..n {
        let shared = shared.clone();
        handles.push(tokio::spawn(async move {
            shared.append(Some(root), user_msg(&format!("m{i}"))).await
        }));
    }
    let results = futures::future::join_all(handles).await;
    let ids: Vec<u64> = results.into_iter().map(|r| r.unwrap().unwrap()).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), n, "并发 append 产生重复 id");

    // load 得到全部节点
    let tree = shared.load().await.unwrap();
    assert_eq!(tree.nodes.len(), n + 1);
    assert_eq!(tree.root, Some(root));
    assert_eq!(tree.nodes[&root].children.len(), n);

    // 无交错半行：逐行解析，全部为完整合法 JSON
    let raw = tokio::fs::read_to_string(&path).await.unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), n + 1);
    for line in &lines {
        let _: SessionEntry = serde_json::from_str(line).unwrap();
    }
}

#[tokio::test]
async fn lane_writer_fork_branches() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let inner = Arc::new(JsonlSessionStorage::open(&path, "s1").await.unwrap());
    let shared = Arc::new(SharedSessionStorage::new(inner));

    let root = shared.append(None, user_msg("root")).await.unwrap();
    let mut lane_a = LaneWriter::new(shared.clone(), "lane-a", Some(root));
    let mut lane_b = LaneWriter::new(shared.clone(), "lane-b", Some(root));
    let id_a = lane_a.append(user_msg("a")).await.unwrap();
    let id_b = lane_b.append(user_msg("b")).await.unwrap();
    assert_ne!(id_a, id_b);

    let tree = shared.load().await.unwrap();
    assert_eq!(tree.nodes.len(), 3);
    let leaves = tree.leaves();
    assert_eq!(leaves.len(), 2);
    assert!(leaves.contains(&id_a));
    assert!(leaves.contains(&id_b));
    assert_eq!(tree.nodes[&root].children.len(), 2);
}

#[tokio::test]
async fn shared_storage_crash_recovery_after_concurrent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let inner = Arc::new(JsonlSessionStorage::open(&path, "s1").await.unwrap());
    let shared = Arc::new(SharedSessionStorage::new(inner));

    let root = shared.append(None, user_msg("root")).await.unwrap();
    let n = 16;
    let mut handles = Vec::new();
    for i in 0..n {
        let shared = shared.clone();
        handles.push(tokio::spawn(async move {
            shared.append(Some(root), user_msg(&format!("m{i}"))).await
        }));
    }
    let results = futures::future::join_all(handles).await;
    for r in results {
        r.unwrap().unwrap();
    }

    // 模拟进程重启：新建 storage 实例（重新 open 读全量恢复游标）
    let reopened = JsonlSessionStorage::open(&path, "s1").await.unwrap();
    assert_eq!(reopened.next_id(), n as u64 + 1);
    let tree = reopened.load().await.unwrap();
    assert_eq!(tree.nodes.len(), n + 1);
    assert_eq!(tree.root, Some(root));
    assert_eq!(tree.nodes[&root].children.len(), n);
}
