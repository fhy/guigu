//! `JsonlSessionStorage` 单元测试（从主文件拆出以控制行数，conventions 体量限制）。

use super::*;
use crate::core::message::{Message, UserContent, UserMessage};
use std::sync::Arc;

fn user_msg(text: &str) -> Message {
    Message::User(UserMessage {
        content: vec![UserContent::Text { text: text.into() }],
        timestamp: 0,
    })
}

/// Task 028：open_locked 启用跨进程锁，append 可正常获取锁并落盘。
#[tokio::test]
async fn test_open_locked_append() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("session.jsonl");
    let lock = FileLock::for_path(&path);
    let storage = JsonlSessionStorage::open_locked(&path, "test-session", lock)
        .await
        .expect("open_locked should succeed");

    let id = storage
        .append(None, user_msg("hello"))
        .await
        .expect("append should succeed");
    assert_eq!(id, 0, "first append should get id 0");

    // load 验证落盘（load 不抢锁）。
    let tree = storage.load().await.expect("load should succeed");
    assert_eq!(tree.nodes.len(), 1, "should have 1 node");
}

/// Task 028：open_locked 顺序 append 正确（跨进程锁每次获取/释放）。
#[tokio::test]
async fn test_open_locked_sequential_append() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("session.jsonl");
    let lock = FileLock::for_path(&path);
    let storage = Arc::new(
        JsonlSessionStorage::open_locked(&path, "test-session", lock)
            .await
            .expect("open_locked should succeed"),
    );

    // 顺序 append 8 条（parent 链），验证每次 append 正确获取/释放跨进程锁。
    let mut parent: Option<u64> = None;
    for i in 0..8 {
        let msg = user_msg(&format!("msg-{i}"));
        let id = storage
            .append(parent, msg)
            .await
            .expect("append should succeed");
        parent = Some(id);
    }

    // load 验证条数正确、无半行、树结构正确。
    let tree = storage.load().await.expect("load should succeed");
    assert_eq!(tree.nodes.len(), 8, "should have 8 nodes");
    assert_eq!(tree.root, Some(0), "root should be id 0");
}

/// 零破坏：open 默认无跨进程锁，行为与 009 一致。
#[tokio::test]
async fn test_open_default_no_file_lock() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("session.jsonl");
    let storage = JsonlSessionStorage::open(&path, "test-session")
        .await
        .expect("open should succeed");

    let id = storage
        .append(None, user_msg("hello"))
        .await
        .expect("append should succeed");
    assert_eq!(id, 0, "first append should get id 0");
}

/// Task 028（Critical 回归）：两个独立实例（同路径、均 `open_locked`）并发 append →
/// id 唯一、记录条数完整（锁内跨进程游标刷新生效，无重复 id / 无丢失消息）。
///
/// 两实例各有独立进程内 `next_id`（均从 0 起）；若锁未覆盖「读游标/分配」，
/// 二者会各自认领 id 0 产生重复。本测试断言最终 id 全唯一且条数 = 2N。
#[tokio::test]
async fn test_open_locked_two_instances_concurrent_append() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("session.jsonl");
    let lock = FileLock::for_path(&path);

    // 两个独立实例（各自进程内 next_id 游标，均从 0 起）。
    let storage_a = Arc::new(
        JsonlSessionStorage::open_locked(path.clone(), "test-session", lock.clone())
            .await
            .expect("open_locked A should succeed"),
    );
    let storage_b = Arc::new(
        JsonlSessionStorage::open_locked(path.clone(), "test-session", lock)
            .await
            .expect("open_locked B should succeed"),
    );

    const N: u64 = 10;
    let mut handles = Vec::new();
    for storage in [&storage_a, &storage_b] {
        let s = Arc::clone(storage);
        handles.push(tokio::spawn(async move {
            for _ in 0..N {
                s.append(None, user_msg("concurrent"))
                    .await
                    .expect("append should succeed");
            }
        }));
    }
    for h in handles {
        h.await.expect("task should complete");
    }

    // 读全量记录：条数完整（2N）、id 全唯一（无跨实例重复）。
    let records = JsonlSessionStorage::read_records(&path)
        .await
        .expect("read_records should succeed");
    let ids: Vec<u64> = records
        .iter()
        .filter_map(|record| match record {
            SessionRecord::Message(entry) => Some(entry.id),
            SessionRecord::LaneHead(_) => None,
        })
        .collect();
    assert_eq!(
        ids.len(),
        (2 * N) as usize,
        "should have 2N message records (no lost writes)"
    );
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        ids.len(),
        "all ids must be unique across instances"
    );
}

/// Task 028：崩溃残留半行修复——文件尾部有半行（崩溃残留），`open_locked` 的
/// append 在锁内截断半行、刷新游标并正常写入（load 条数正确、无半行、树有效）。
#[tokio::test]
async fn test_open_locked_truncates_crash_half_line() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("session.jsonl");

    // 手工构造：1 条有效行 + 1 条崩溃半行（不完整 JSON、无尾部换行）。
    let valid_line = serde_json::to_string(&SessionEntry {
        id: 0,
        parent_id: None,
        message: user_msg("valid"),
    })
    .expect("serialize valid line");
    let half_line = "{\"id\":1,\"parent_id\":null,\"message\":{"; // 不完整 JSON
    std::fs::write(&path, format!("{valid_line}\n{half_line}"))
        .expect("write file with trailing half line");

    let lock = FileLock::for_path(&path);
    let storage = JsonlSessionStorage::open_locked(&path, "test-session", lock)
        .await
        .expect("open_locked should succeed");

    // append：锁内截断半行、刷新游标（max_id=0 → next=1）、认领 id 1。
    let id = storage
        .append(Some(0), user_msg("after-crash"))
        .await
        .expect("append should succeed");
    assert_eq!(id, 1, "should claim id 1 (after valid id 0)");

    // load：半行已被截断，2 条完整行，树有效（单根、无孤儿）。
    let tree = storage.load().await.expect("load should succeed");
    assert_eq!(tree.nodes.len(), 2, "should have 2 nodes (valid + new)");
    assert_eq!(tree.root, Some(0), "root should be id 0");
}
