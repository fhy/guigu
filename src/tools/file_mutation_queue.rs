//! FileMutationQueue：进程内、跨 agent 的 per-path 异步写锁。
//!
//! 多个 `AgentRuntime` 实例（多 agent）共享同一进程、写同一文件时，003 主循环的
//! 单 agent 编排无法覆盖。本队列以规范化路径为 key 提供 per-path 互斥：不同路径
//! 并行、同一路径串行。
//!
//! 已知局限（规格接受，后续任务再补）：
//! - 一期不解析 symlink/hardlink：同一物理文件经不同路径可能漏串行化。
//!
//! 锁表回收（017-c）：**惰性驱逐**——`strong_count == 1`（仅锁表持有一份强引用，
//! 无 in-flight acquire / 持有中 guard）的条目，在 `acquire` 达 [`PRUNE_THRESHOLD`]
//! 阈值时或显式 [`FileMutationQueue::prune`] 时回收。正确性依据：`acquire` 的
//! 「克隆 Arc」与 `prune` 的「check `strong_count` + remove」都在同一把锁表锁内
//! 串行，二者原子，故单段「锁内 check-and-remove」即安全，无需两阶段 dying 态。

use std::collections::HashMap;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::core::file_lock::{FileLock, FileLockGuard};

/// 锁表自动驱逐阈值：`acquire` 持表锁后、插入前，若条目数 `>=` 该值则先驱逐
/// 一次 `strong_count == 1` 的条目（见模块文档的回收策略）。
pub const PRUNE_THRESHOLD: usize = 1024;

/// per-path 异步写锁表。
///
/// 惰性为每个 path 建锁；锁表的并发访问用 `std::sync::Mutex`（操作极短、不跨
/// await）。锁表惰性驱逐：`strong_count == 1` 的条目在阈值触发或显式
/// [`FileMutationQueue::prune`] 时回收（见模块文档）。
///
/// Task 028：可选叠加跨进程锁（[`FileMutationQueue::with_file_lock`]）。启用后
/// `acquire` 在进程内 per-path 锁之后、IO 之前，按目标路径动态构造
/// [`FileLock::for_path`] 并获取跨进程独占锁，实现同路径跨进程串行。
#[derive(Debug, Default)]
pub struct FileMutationQueue {
    locks: std::sync::Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
    /// Task 028：跨进程锁开关。`true` 时 `acquire` 叠加跨进程锁。
    file_lock_enabled: bool,
}

impl FileMutationQueue {
    /// 创建空锁表（仅进程内 per-path 锁，行为与 006 完全一致）。
    pub fn new() -> Self {
        FileMutationQueue {
            locks: std::sync::Mutex::new(HashMap::new()),
            file_lock_enabled: false,
        }
    }

    /// 创建带跨进程锁的锁表（Task 028）：进程内 per-path 锁 + 跨进程锁双层。
    ///
    /// 每个 `acquire(path)` 在拿到进程内锁后，按目标路径动态构造
    /// [`FileLock::for_path(path)`] 并获取跨进程独占锁。不同路径互不干扰、
    /// 同路径跨进程互斥。
    ///
    /// 注：规格原稿签名 `with_file_lock(lock: FileLock)` 与正文 per-path 设计
    /// 矛盾（单个 `FileLock` 无法覆盖多路径），故采用无参数签名。
    pub fn with_file_lock() -> Self {
        FileMutationQueue {
            locks: std::sync::Mutex::new(HashMap::new()),
            file_lock_enabled: true,
        }
    }

    /// 获取 `path` 的写锁；不同 path 可并行，同一 path 串行。
    ///
    /// 等待期间可被外层 `tokio::select!` + `signal.cancelled()` 打断（本方法自身
    /// 不绑定取消）。返回的 guard 持有锁，Drop 自动释放。
    ///
    /// 持表锁后、插入前若条目数达 [`PRUNE_THRESHOLD`]，先执行一次锁内驱逐
    /// （复用 [`FileMutationQueue::prune_locked`]，避免递归获取表锁）。
    ///
    /// Task 028：启用跨进程锁（[`FileMutationQueue::with_file_lock`]）时，在拿到
    /// 进程内锁之后、IO 之前，按目标路径动态构造 [`FileLock::for_path`] 并获取
    /// 跨进程独占锁。跨进程锁获取失败时记录错误并继续（进程内锁仍持有，队列仍
    /// 进程内安全），不破坏 `acquire` 的既有签名。
    pub async fn acquire(&self, path: &Path) -> FileMutationGuard<'_> {
        let key = normalize(path);
        // 锁表操作极短：取/建 Arc 后立即释放 std Mutex，不跨 await。
        let lock = {
            let mut table = self.locks.lock().unwrap_or_else(|e| e.into_inner());
            if table.len() >= PRUNE_THRESHOLD {
                self.prune_locked(&mut table);
            }
            table
                .entry(key)
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let inner = lock.lock_owned().await;
        // Task 028：跨进程锁（仅当启用时）。在进程内锁之后、IO 之前获取。
        // `key` 已移入锁表，此处用 `path` 重新规范化（与锁 key 一致）。
        let file_guard = if self.file_lock_enabled {
            let lock_key = normalize(path);
            match FileLock::for_path(&lock_key).lock_exclusive().await {
                Ok(guard) => Some(guard),
                Err(err) => {
                    tracing::error!(
                        "cross-process file lock failed for {}: {err}",
                        lock_key.display()
                    );
                    None
                }
            }
        } else {
            None
        };
        FileMutationGuard {
            _inner: inner,
            _file: file_guard,
            _phantom: PhantomData,
        }
    }

    /// 显式驱逐锁表中 `strong_count == 1`（仅锁表持有一份强引用）的条目。
    ///
    /// 有 in-flight acquire 或持有中 guard 的 path 不会被驱逐（`strong_count >= 2`）；
    /// 被驱逐的 path 后续 `acquire` 会新建锁，互斥语义不变（见模块文档）。
    pub fn prune(&self) {
        let mut table = self.locks.lock().unwrap_or_else(|e| e.into_inner());
        self.prune_locked(&mut table);
    }

    /// 锁内驱逐实现：调用方必须已持有表锁（`prune` 与 `acquire` 的阈值路径复用，
    /// 避免在已持 `std::sync::Mutex` 时递归获取导致死锁）。
    fn prune_locked(&self, table: &mut HashMap<PathBuf, Arc<Mutex<()>>>) {
        table.retain(|_, lock| Arc::strong_count(lock) > 1);
    }

    /// 锁表当前条目数（诊断 / 测试用）。
    pub fn len(&self) -> usize {
        self.locks.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// 锁表是否为空（诊断 / 测试用）。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
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
///
/// Task 028：可选持 `FileLockGuard`（跨进程锁），Drop 时自动解锁。
pub struct FileMutationGuard<'a> {
    _inner: OwnedMutexGuard<()>,
    /// Task 028：跨进程锁 guard（仅 `with_file_lock` 启用时 `Some`）。
    _file: Option<FileLockGuard>,
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

    /// prune：全部 guard drop 后 `prune()` 清空锁表（`len == 0`）。
    #[tokio::test]
    async fn test_prune_clears_released_paths() {
        let queue = Arc::new(FileMutationQueue::new());
        let path = PathBuf::from("/tmp/guigu-queue/prune-empty.txt");
        {
            let _g = queue.acquire(&path).await;
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
        let _g_a = queue.acquire(&path_a).await;
        let _g_b = queue.acquire(&path_b).await;
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
            let _g = queue.acquire(&path).await;
        }
        assert_eq!(queue.len(), PRUNE_THRESHOLD);
        // 新 path 的 acquire 触发阈值驱逐：旧条目全被回收，仅留新条目。
        let new_path = PathBuf::from("/tmp/guigu-queue/auto-new.txt");
        let _g = queue.acquire(&new_path).await;
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
            let _guard = queue.acquire(&path).await;
        }
        // guard Drop 后跨进程锁释放，再次 acquire 立即可得。
        let start = Instant::now();
        let _guard = queue.acquire(&path).await;
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
        let guard_a = queue.acquire(&path_a).await;
        let guard_b = queue.acquire(&path_b).await;
        drop(guard_a);
        drop(guard_b);
    }

    /// 回归：驱逐后同 path 再 acquire 仍互斥（新建锁不破坏串行化）。
    #[tokio::test]
    async fn test_mutex_preserved_after_eviction() {
        let queue = Arc::new(FileMutationQueue::new());
        let path = PathBuf::from("/tmp/guigu-queue/evict-mutex.txt");
        {
            let _g = queue.acquire(&path).await;
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
            "same path must stay serialized after eviction"
        );
    }
}
