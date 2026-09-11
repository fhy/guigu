//! `FileMutationQueue` 单元测试（从主文件拆出以控制行数，conventions 体量限制）。

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// 同一 path 并发 acquire 串行：任意时刻临界区内 ≤1。
#[tokio::test]
async fn test_same_path_serialized() {
    let queue = Arc::new(FileMutationQueue::new());
    let path = PathBuf::from("/tmp/guigu-queue/same.txt");
    let current = Arc::new(AtomicUsize::new(0));
    let max_seen = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..8 {
        let q = Arc::clone(&queue);
        let p = path.clone();
        let current = Arc::clone(&current);
        let max_seen = Arc::clone(&max_seen);
        handles.push(tokio::spawn(async move {
            let _guard = q.acquire(&p).await.expect("acquire should succeed");
            let now = current.fetch_add(1, Ordering::SeqCst) + 1;
            max_seen.fetch_max(now, Ordering::SeqCst);
            tokio::task::yield_now().await;
            current.fetch_sub(1, Ordering::SeqCst);
        }));
    }
    for h in handles {
        h.await.expect("task should complete");
    }
    assert_eq!(
        max_seen.load(Ordering::SeqCst),
        1,
        "same path must be serialized"
    );
}

/// 不同 path 可并行：两个临界区可同时进入。
#[tokio::test]
async fn test_different_paths_parallel() {
    let queue = Arc::new(FileMutationQueue::new());
    let path_a = PathBuf::from("/tmp/guigu-queue/a.txt");
    let path_b = PathBuf::from("/tmp/guigu-queue/b.txt");
    let a_in = Arc::new(AtomicBool::new(false));
    let b_in = Arc::new(AtomicBool::new(false));

    let q1 = Arc::clone(&queue);
    let a_in1 = Arc::clone(&a_in);
    let h1 = tokio::spawn(async move {
        let _g = q1.acquire(&path_a).await.expect("acquire should succeed");
        a_in1.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(100)).await;
        a_in1.store(false, Ordering::SeqCst);
    });

    let q2 = Arc::clone(&queue);
    let b_in1 = Arc::clone(&b_in);
    let h2 = tokio::spawn(async move {
        let _g = q2.acquire(&path_b).await.expect("acquire should succeed");
        b_in1.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(100)).await;
        b_in1.store(false, Ordering::SeqCst);
    });

    // 等两个任务都进入临界区（不同 path 应能同时进入）。
    for _ in 0..200 {
        if a_in.load(Ordering::SeqCst) && b_in.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        a_in.load(Ordering::SeqCst) && b_in.load(Ordering::SeqCst),
        "different paths should be parallel"
    );
    h1.await.expect("task a should complete");
    h2.await.expect("task b should complete");
}

/// guard Drop 后锁可被再次 acquire（RAII 释放）。
#[tokio::test]
async fn test_guard_drop_releases() {
    let queue = Arc::new(FileMutationQueue::new());
    let path = PathBuf::from("/tmp/guigu-queue/drop.txt");
    {
        let _g = queue.acquire(&path).await.expect("acquire should succeed");
    }
    let start = Instant::now();
    let _g = queue.acquire(&path).await.expect("acquire should succeed");
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "acquire after drop should be immediate"
    );
}

/// acquire 等待可被外层 select + signal 取消。
#[tokio::test]
async fn test_acquire_cancelled_by_select() {
    let queue = Arc::new(FileMutationQueue::new());
    let path = PathBuf::from("/tmp/guigu-queue/cancel.txt");
    let holder_started = Arc::new(AtomicBool::new(false));

    // 后台任务先拿到锁并持有 300ms。
    let q_holder = Arc::clone(&queue);
    let p_holder = path.clone();
    let started = Arc::clone(&holder_started);
    let holder = tokio::spawn(async move {
        let _g = q_holder
            .acquire(&p_holder)
            .await
            .expect("acquire should succeed");
        started.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(300)).await;
    });

    // 等 holder 确实持锁，保证主任务的 acquire 必然进入等待。
    for _ in 0..200 {
        if holder_started.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        holder_started.load(Ordering::SeqCst),
        "holder should hold the lock"
    );

    let signal = CancellationToken::new();
    let sig2 = signal.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        sig2.cancel();
    });
    let won = tokio::select! {
        _g = queue.acquire(&path) => true,
        _ = signal.cancelled() => false,
    };
    assert!(!won, "acquire should be cancelled by outer select");
    holder.await.expect("holder should complete");
}

/// prune：全部 guard drop 后 `prune()` 清空锁表（`len == 0`）。
#[tokio::test]
async fn test_prune_clears_released_paths() {
    let queue = Arc::new(FileMutationQueue::new());
    let path = PathBuf::from("/tmp/guigu-queue/prune-empty.txt");
    {
        let _g = queue.acquire(&path).await.expect("acquire should succeed");
    }
    assert_eq!(queue.len(), 1, "entry should exist before prune");
    queue.prune();
    assert_eq!(
        queue.len(),
        0,
        "prune should clear entries with no in-flight refs"
    );
}

/// prune：有 in-flight guard 的 path 不被驱逐（`strong_count >= 2`）。
#[tokio::test]
async fn test_prune_keeps_inflight_path() {
    let queue = Arc::new(FileMutationQueue::new());
    let path_a = PathBuf::from("/tmp/guigu-queue/prune-a.txt");
    let path_b = PathBuf::from("/tmp/guigu-queue/prune-b.txt");
    let _g_a = queue
        .acquire(&path_a)
        .await
        .expect("acquire should succeed");
    let _g_b = queue
        .acquire(&path_b)
        .await
        .expect("acquire should succeed");
    queue.prune();
    assert_eq!(queue.len(), 2, "in-flight paths must not be evicted");
    drop(_g_b);
    queue.prune();
    assert_eq!(queue.len(), 1, "released path evicted, in-flight kept");
    drop(_g_a);
    queue.prune();
    assert_eq!(queue.len(), 0);
}

/// 自动驱逐：`acquire` 达阈值时先驱逐 `strong_count == 1` 条目再插入。
#[tokio::test]
async fn test_acquire_auto_prunes_at_threshold() {
    let queue = Arc::new(FileMutationQueue::new());
    // 填满至阈值：每 path acquire 一次并 drop guard（strong_count 归 1）。
    for i in 0..PRUNE_THRESHOLD {
        let path = PathBuf::from(format!("/tmp/guigu-queue/auto-{i}.txt"));
        let _g = queue.acquire(&path).await.expect("acquire should succeed");
    }
    assert_eq!(queue.len(), PRUNE_THRESHOLD);
    // 新 path 的 acquire 触发阈值驱逐：旧条目全被回收，仅留新条目。
    let new_path = PathBuf::from("/tmp/guigu-queue/auto-new.txt");
    let _g = queue
        .acquire(&new_path)
        .await
        .expect("acquire should succeed");
    assert_eq!(
        queue.len(),
        1,
        "auto-prune should evict all stale entries before insert"
    );
}

/// Task 028：with_file_lock 启用跨进程锁，acquire 可正常获取与释放。
#[tokio::test]
async fn test_with_file_lock_acquire_and_release() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("locked.txt");
    let queue = Arc::new(FileMutationQueue::with_file_lock());

    // 首次 acquire 成功（跨进程锁 + 进程内锁均获取）。
    {
        let _guard = queue.acquire(&path).await.expect("acquire should succeed");
    }
    // guard Drop 后跨进程锁释放，再次 acquire 立即可得。
    let start = Instant::now();
    let _guard = queue.acquire(&path).await.expect("acquire should succeed");
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "acquire after drop should be immediate"
    );
}

/// Task 028：with_file_lock 同 path 并发 acquire 串行（进程内 + 跨进程双层）。
#[tokio::test]
async fn test_with_file_lock_same_path_serialized() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("serial.txt");
    let queue = Arc::new(FileMutationQueue::with_file_lock());
    let current = Arc::new(AtomicUsize::new(0));
    let max_seen = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..4 {
        let q = Arc::clone(&queue);
        let p = path.clone();
        let current = Arc::clone(&current);
        let max_seen = Arc::clone(&max_seen);
        handles.push(tokio::spawn(async move {
            let _guard = q.acquire(&p).await.expect("acquire should succeed");
            let now = current.fetch_add(1, Ordering::SeqCst) + 1;
            max_seen.fetch_max(now, Ordering::SeqCst);
            tokio::task::yield_now().await;
            current.fetch_sub(1, Ordering::SeqCst);
        }));
    }
    for h in handles {
        h.await.expect("task should complete");
    }
    assert_eq!(
        max_seen.load(Ordering::SeqCst),
        1,
        "same path must be serialized with file lock"
    );
}

/// Task 028：with_file_lock 不同 path 可并行（跨进程锁按路径隔离）。
#[tokio::test]
async fn test_with_file_lock_different_paths_parallel() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path_a = dir.path().join("a.txt");
    let path_b = dir.path().join("b.txt");
    let queue = Arc::new(FileMutationQueue::with_file_lock());

    // 两个不同 path 的 guard 可同时持有（跨进程锁按路径隔离）。
    let guard_a = queue
        .acquire(&path_a)
        .await
        .expect("acquire should succeed");
    let guard_b = queue
        .acquire(&path_b)
        .await
        .expect("acquire should succeed");
    drop(guard_a);
    drop(guard_b);
}

/// 回归：驱逐后同 path 再 acquire 仍互斥（新建锁不破坏串行化）。
#[tokio::test]
async fn test_mutex_preserved_after_eviction() {
    let queue = Arc::new(FileMutationQueue::new());
    let path = PathBuf::from("/tmp/guigu-queue/evict-mutex.txt");
    {
        let _g = queue.acquire(&path).await.expect("acquire should succeed");
    }
    queue.prune();
    assert_eq!(queue.len(), 0, "path evicted");
    // 驱逐后并发 acquire 同 path：任意时刻临界区内 ≤1。
    let current = Arc::new(AtomicUsize::new(0));
    let max_seen = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for _ in 0..4 {
        let q = Arc::clone(&queue);
        let p = path.clone();
        let current = Arc::clone(&current);
        let max_seen = Arc::clone(&max_seen);
        handles.push(tokio::spawn(async move {
            let _guard = q.acquire(&p).await.expect("acquire should succeed");
            let now = current.fetch_add(1, Ordering::SeqCst) + 1;
            max_seen.fetch_max(now, Ordering::SeqCst);
            tokio::task::yield_now().await;
            current.fetch_sub(1, Ordering::SeqCst);
        }));
    }
    for h in handles {
        h.await.expect("task should complete");
    }
    assert_eq!(
        max_seen.load(Ordering::SeqCst),
        1,
        "same path must stay serialized after eviction"
    );
}

/// Task 028（High 回归）：跨进程锁无法获取（锁文件父目录不存在 → 无法创建锁文件）
/// 时，`acquire` 返回 `Err` 并拒绝进入写临界区（不静默退化为仅进程内锁）。
#[tokio::test]
async fn test_with_file_lock_acquire_fails_when_lock_unavailable() {
    let dir = tempfile::tempdir().expect("tempdir");
    // 目标路径父目录不存在 → 锁文件无法创建 → lock_exclusive 返回 Err(Open)。
    let path = dir.path().join("nonexistent-dir/file.txt");
    let queue = Arc::new(FileMutationQueue::with_file_lock());

    let result = queue.acquire(&path).await;
    assert!(
        result.is_err(),
        "acquire should fail when the cross-process lock cannot be acquired"
    );
    match result {
        Err(FileMutationError::FileLock(_)) => {} // 预期：跨进程锁错误
        Ok(_) => panic!("acquire should not succeed when the lock is unavailable"),
    }
}
