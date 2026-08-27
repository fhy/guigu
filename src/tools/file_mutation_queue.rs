//! FileMutationQueue：进程内、跨 agent 的 per-path 异步写锁。
//!
//! 多个 `AgentRuntime` 实例（多 agent）共享同一进程、写同一文件时，003 主循环的
//! 单 agent 编排无法覆盖。本队列以规范化路径为 key 提供 per-path 互斥：不同路径
//! 并行、同一路径串行。
//!
//! 已知局限（规格接受，后续任务再补）：
//! - 一期不解析 symlink/hardlink：同一物理文件经不同路径可能漏串行化。
//! - 一期锁表只增不减：安全驱逐需两阶段 dying 态或代际计数（否则 A drop 后查
//!   `strong_count==1` 准备移除时，B 并发克隆旧 Arc 持旧锁、C 建新锁，互斥被破坏）。
//!   条目小（约 100–200B）、agent 触碰路径通常有界，故接受无界增长。

use std::collections::HashMap;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::{Mutex, OwnedMutexGuard};

/// per-path 异步写锁表。
///
/// 惰性为每个 path 建锁；锁表的并发访问用 `std::sync::Mutex`（操作极短、不跨
/// await）。一期锁表只增不减（见模块文档的已知局限）。
#[derive(Debug, Default)]
pub struct FileMutationQueue {
    locks: std::sync::Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
}

impl FileMutationQueue {
    /// 创建空锁表。
    pub fn new() -> Self {
        FileMutationQueue {
            locks: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// 获取 `path` 的写锁；不同 path 可并行，同一 path 串行。
    ///
    /// 等待期间可被外层 `tokio::select!` + `signal.cancelled()` 打断（本方法自身
    /// 不绑定取消）。返回的 guard 持有锁，Drop 自动释放。
    pub async fn acquire(&self, path: &Path) -> FileMutationGuard<'_> {
        let key = normalize(path);
        // 锁表操作极短：取/建 Arc 后立即释放 std Mutex，不跨 await。
        let lock = {
            let mut table = self.locks.lock().unwrap_or_else(|e| e.into_inner());
            table
                .entry(key)
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let inner = lock.lock_owned().await;
        FileMutationGuard {
            _inner: inner,
            _phantom: PhantomData,
        }
    }
}

/// 以规范化路径作锁 key：`std::path::absolute`，失败退回原始 `PathBuf`。
/// 一期不解析 symlink/hardlink（已知局限）。
fn normalize(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// 写锁 guard：Drop 即释放（RAII），覆盖异常/取消提前返回路径。
///
/// `Send`，可在文件 IO 的 `await` 期间持有。内部持 `OwnedMutexGuard`（owned，
/// 使 guard 生命周期不依赖锁表项存活）。
pub struct FileMutationGuard<'a> {
    _inner: OwnedMutexGuard<()>,
    _phantom: PhantomData<&'a ()>,
}

#[cfg(test)]
mod tests {
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
                let _guard = q.acquire(&p).await;
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
            let _g = q1.acquire(&path_a).await;
            a_in1.store(true, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(100)).await;
            a_in1.store(false, Ordering::SeqCst);
        });

        let q2 = Arc::clone(&queue);
        let b_in1 = Arc::clone(&b_in);
        let h2 = tokio::spawn(async move {
            let _g = q2.acquire(&path_b).await;
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
            let _g = queue.acquire(&path).await;
        }
        let start = Instant::now();
        let _g = queue.acquire(&path).await;
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
            let _g = q_holder.acquire(&p_holder).await;
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
}
