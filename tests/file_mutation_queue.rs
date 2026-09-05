//! Task 006 集成测试：FileMutationQueue 跨 agent 同文件写串行化。
//!
//! 两个 `WriteTool` 共享同一 `Arc<FileMutationQueue>` 并发写同一路径，验证写
//! IO 在 guard 持有期间执行、同路径串行（无交错/损坏）。临时目录用
//! `std::env::temp_dir()` + 唯一后缀，不硬编码路径，测试结束清理。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use guigu::core::tool::Tool;
use guigu::tools::{FileMutationQueue, WriteTool};
use tokio_util::sync::CancellationToken;

/// 生成唯一临时目录（进程 id + 计数器），保证测试间隔离。
fn temp_dir_unique() -> std::path::PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!("guigu-queue-{}-{}", std::process::id(), n))
}

/// 两个 WriteTool 共享同一 queue 并发写同一路径：均成功且文件为某次完整写
/// （无交错/损坏），验证跨 agent 同文件写串行化。
#[tokio::test]
async fn test_write_same_path_serialized_via_shared_queue() {
    let queue = Arc::new(FileMutationQueue::new());
    let dir = temp_dir_unique();
    let path = dir.join("shared.txt");

    let tool1 = Arc::new(WriteTool::new(Arc::clone(&queue), None));
    let tool2 = Arc::new(WriteTool::new(Arc::clone(&queue), None));
    let p1 = path.to_string_lossy().to_string();
    let p2 = path.to_string_lossy().to_string();

    let h1 = tokio::spawn(async move {
        tool1
            .execute(
                "c1",
                serde_json::json!({ "path": p1, "content": "one" }),
                CancellationToken::new(),
                None,
            )
            .await
    });
    let h2 = tokio::spawn(async move {
        tool2
            .execute(
                "c2",
                serde_json::json!({ "path": p2, "content": "two" }),
                CancellationToken::new(),
                None,
            )
            .await
    });

    let r1 = h1
        .await
        .expect("join write1")
        .expect("write1 should succeed");
    let r2 = h2
        .await
        .expect("join write2")
        .expect("write2 should succeed");
    assert!(!r1.is_error && !r2.is_error, "both writes should succeed");

    let on_disk = std::fs::read_to_string(&path).expect("file should exist");
    assert!(
        on_disk == "one" || on_disk == "two",
        "file should hold one complete write, got: {on_disk:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
