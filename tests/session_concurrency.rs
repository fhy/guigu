//! Task 017-a 集成测试：每 lane 多步连续写并发（星型 → 链式）。
//!
//! 从 `tests/session.rs` 拆出（单文件 ≤ 400 行约束）：两 lane 共享同一
//! `Arc<SharedSessionStorage>`，从同一初始 head 各自**连续** append 3 条，验证
//! 链式结构、共同 parent、id 唯一与 JSONL 无交错半行。共享 helper 见 `tests/common`。

mod common;

use std::sync::Arc;

use common::user_msg;
use guigu::core::session::{
    JsonlSessionStorage, LaneWriter, SessionEntry, SessionStorage, SharedSessionStorage,
};

/// 两 lane 共享同一 `Arc<SharedSessionStorage>`，从同一初始 head（root）各自**连续**
/// append 3 条（`join_all` 并发跑两 lane，lane 内顺序 await）。
///
/// 断言：root + 3 + 3 = 7 节点；每 lane 3 条成单链（第 i+1 条 parent = 第 i 条）；
/// 两链首节点互为兄弟（同 parent = root）；id 全局唯一；JSONL 无交错半行。
#[tokio::test]
async fn lane_writer_multi_step_concurrent_chains() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let inner = Arc::new(JsonlSessionStorage::open(&path, "s1").await.unwrap());
    let shared = Arc::new(SharedSessionStorage::new(inner));

    // 建根（初始 head），两 lane 从同一 head 各自连续 append 3 条。
    let root = shared.append(None, user_msg("root")).await.unwrap();

    // 两 lane 并发跑（join_all），lane 内顺序 await。
    let handle_a = {
        let shared = shared.clone();
        tokio::spawn(async move {
            let mut lane = LaneWriter::new(shared, "lane-a", Some(root));
            let a0 = lane.append(user_msg("a0")).await.unwrap();
            let a1 = lane.append(user_msg("a1")).await.unwrap();
            let a2 = lane.append(user_msg("a2")).await.unwrap();
            (a0, a1, a2)
        })
    };
    let handle_b = {
        let shared = shared.clone();
        tokio::spawn(async move {
            let mut lane = LaneWriter::new(shared, "lane-b", Some(root));
            let b0 = lane.append(user_msg("b0")).await.unwrap();
            let b1 = lane.append(user_msg("b1")).await.unwrap();
            let b2 = lane.append(user_msg("b2")).await.unwrap();
            (b0, b1, b2)
        })
    };
    let results = futures::future::join_all(vec![handle_a, handle_b]).await;
    let mut iter = results.into_iter();
    let (a0, a1, a2) = iter.next().unwrap().unwrap();
    let (b0, b1, b2) = iter.next().unwrap().unwrap();

    // load 得到全部节点（root + 3 + 3 = 7）。
    let tree = shared.load().await.unwrap();
    assert_eq!(tree.nodes.len(), 7);
    assert_eq!(tree.root, Some(root));

    // 每个 lane 的 3 条消息形成各自的单链（第 i+1 条 parent = 第 i 条）。
    assert_eq!(tree.nodes[&a0].parent_id, Some(root));
    assert_eq!(tree.nodes[&a1].parent_id, Some(a0));
    assert_eq!(tree.nodes[&a2].parent_id, Some(a1));
    assert_eq!(tree.nodes[&b0].parent_id, Some(root));
    assert_eq!(tree.nodes[&b1].parent_id, Some(b0));
    assert_eq!(tree.nodes[&b2].parent_id, Some(b1));

    // 两条链的第一条互为兄弟（同 parent = root）。
    assert_eq!(tree.nodes[&a0].parent_id, tree.nodes[&b0].parent_id);
    assert_eq!(tree.nodes[&root].children.len(), 2);

    // id 全局唯一。
    let ids: Vec<u64> = tree.nodes.keys().copied().collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len(), "id 全局唯一");

    // 无交错半行：逐行解析，全部为完整合法 JSON。
    let raw = tokio::fs::read_to_string(&path).await.unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 7);
    for line in &lines {
        let _: SessionEntry = serde_json::from_str(line).unwrap();
    }
}
