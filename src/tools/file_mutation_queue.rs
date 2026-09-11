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
//!
//! Task 028：可选叠加跨进程锁（见 [`FileMutationQueue::with_file_lock`]）。启用后
//! `acquire` 返回 `Result`：跨进程锁获取失败时**拒绝进入写临界区**（返回 `Err`），
//! 不静默退化为仅进程内锁（保证 opt-in 模式的跨进程串行语义）。

use std::collections::HashMap;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::core::file_lock::{FileLock, FileLockError, FileLockGuard};

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

/// 文件变更错误（Task 028）。
#[derive(Debug, thiserror::Error)]
pub enum FileMutationError {
    /// 跨进程文件锁获取失败（仅 `with_file_lock` 启用时可能）。
    ///
    /// 出现该错误时 `acquire` 拒绝进入写临界区（不静默退化为仅进程内锁），保证
    /// opt-in 模式的跨进程串行语义。保留 `FileLockError` 类型与 source 链。
    #[error("cross-process file lock failed: {0}")]
    FileLock(#[from] FileLockError),
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
    /// 跨进程独占锁。**跨进程锁获取失败时返回 `Err`，拒绝进入写临界区**（不静默
    /// 退化为仅进程内锁，保证 opt-in 模式的跨进程串行语义）。未启用时恒返回
    /// `Ok`（行为与 006 一致）。
    pub async fn acquire(&self, path: &Path) -> Result<FileMutationGuard<'_>, FileMutationError> {
        let key = normalize(path);
        // 跨进程锁用（`key` 下方移入锁表，先克隆一份）。
        let lock_key = key.clone();
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
        // Task 028：跨进程锁（仅当启用时）。在进程内锁之后、IO 之前获取；失败返回
        // `Err`（拒绝进入写临界区），不静默继续。
        let file_guard = if self.file_lock_enabled {
            Some(FileLock::for_path(&lock_key).lock_exclusive().await?)
        } else {
            None
        };
        Ok(FileMutationGuard {
            _inner: inner,
            _file: file_guard,
            _phantom: PhantomData,
        })
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
mod tests;
