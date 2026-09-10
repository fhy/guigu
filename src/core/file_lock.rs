//! 跨进程文件锁（Task 028）。
//!
//! `FileLock` 基于 `fs2`（Unix `flock` / Windows `LockFileEx`）提供跨进程独占锁。
//! 锁文件路径标识互斥域；所有进程对同一锁文件路径互斥。
//!
//! 关键语义：
//! - **锁文件策略**：锁文件路径 = 目标文件 + 后缀 `.guigu.lock`（如 `a.txt` →
//!   `a.txt.guigu.lock`）。多个进程写同一目标文件时，它们各自用同一锁文件路径 →
//!   天然互斥。锁文件由 `fs2` 自动创建（`OpenOptions::create(true)`），不参与业务
//!   数据读写。
//! - **阻塞 + 可取消 + 可超时**：`lock_exclusive` 内部用 `spawn_blocking` 跑
//!   `fs2::FileExt::lock_exclusive`（阻塞 syscall）；调用方用 `tokio::select!` 与
//!   `signal.cancelled()` / `sleep(timeout)` 组合实现取消与超时。**本方法不内置超时**
//!   （对齐 006 `FileMutationQueue::acquire` 的「等待可被外层 select 打断」契约）。
//! - **RAII + 崩溃释放**：`FileLockGuard` Drop 即 `fs2::unlock`，覆盖异常/取消提前
//!   返回；进程崩溃（含 `kill -9`）则内核释放 flock/LockFileEx，无需清理锁文件。
//! - **spawn_blocking 细节**：锁操作（`lock_exclusive`/`try_lock`）均在
//!   `spawn_blocking` 内执行；guard 持有的是**已加锁的 `File` 句柄**（`Arc<File>`），
//!   Drop 时的 `unlock` 是同步快速操作，直接调用即可，不必再 `spawn_blocking`。
//! - **不跨 await 持 std 锁**：`FileLockGuard` 内部仅持 `File` 句柄 + 生命周期借用，
//!   无 std Mutex，天然满足「不持锁跨 await」。
//!
//! 边界声明（明确不做）：
//! - **锁粒度 = 整个文件**（独占锁）：不做字节范围锁、不做读写锁降级。
//! - **不做锁租约/心跳/自动过期**：flock 无 TTL；进程崩溃由内核兜底释放，故无需租约。
//! - **NFS 网络文件系统不支持**：flock 在 NFS 上的语义历史不可靠，声明为不支持。
//! - **Windows 语义**：fs2 已封装 LockFileEx，语义声明支持；但 CI 测试环境假设
//!   Linux，**测试仅保证 Unix**，Windows 以 fs2 文档为准（已知边界）。
//! - **不做分布式锁 / 跨主机协调**（Redis/etcd）：本任务仅进程间文件锁。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fs2::FileExt;

/// 跨进程独占锁：以锁文件路径标识互斥域，所有进程对同一锁文件路径互斥。
///
/// [`FileLock::for_path`] 构造锁（不立即加锁）。锁文件与目标文件同级目录、命名
/// `<目标文件名>.guigu.lock`。
#[derive(Debug, Clone)]
pub struct FileLock {
    lock_file: PathBuf,
}

/// RAII guard：持有已加锁的 `File` 句柄，`Drop` 自动解锁。
///
/// `Send + Sync + Unpin`，可在文件 IO 的 `await` 期间持有。内部持 `Arc<File>`
/// （owned，guard 生命周期不依赖 `FileLock` 存活——规格原稿 `FileLockGuard<'a>`
/// 借用 `&FileLock`，但 `FileMutationQueue::acquire` 中 `FileLock` 为局部变量，
/// guard 需存入跨 await 的 `FileMutationGuard`，故去掉生命周期参数）。
pub struct FileLockGuard {
    _file: Arc<std::fs::File>,
}

/// 文件锁错误。
#[derive(Debug, thiserror::Error)]
pub enum FileLockError {
    /// 打开锁文件失败。
    #[error("failed to open lock file `{path}`: {source}")]
    Open {
        path: String,
        #[source]
        source: std::io::Error,
    },
    /// 获取锁失败。
    #[error("failed to acquire lock on `{path}`: {source}")]
    Acquire {
        path: String,
        #[source]
        source: std::io::Error,
    },
    /// 外层 select 在 spawn_blocking 完成前取消。
    #[error("lock task cancelled")]
    Cancelled,
    /// spawn_blocking 任务 panic/join 失败。
    #[error("join error: {0}")]
    Join(String),
}

impl FileLock {
    /// 构造锁（不立即加锁）。锁文件与目标文件同级目录、命名 `<目标文件名>.guigu.lock`
    /// （如 `a.txt` → `a.txt.guigu.lock`、`session.jsonl` → `session.jsonl.guigu.lock`）。
    pub fn for_path(target: &Path) -> FileLock {
        let mut lock_file = target.to_path_buf();
        if let Some(file_name) = target.file_name() {
            let mut new_name = file_name.to_os_string();
            new_name.push(".guigu.lock");
            lock_file.set_file_name(new_name);
        }
        FileLock { lock_file }
    }

    /// 阻塞获取独占锁（`spawn_blocking` 包裹 fs2 锁调用，可被外层 select 取消/超时）。
    ///
    /// 返回的 guard 持有锁，`Drop` 自动释放。本方法不内置超时——调用方用
    /// `tokio::select!` 与 `signal.cancelled()` / `sleep(timeout)` 组合实现取消与超时。
    pub async fn lock_exclusive(&self) -> Result<FileLockGuard, FileLockError> {
        let lock_file = self.lock_file.clone();
        let result = tokio::task::spawn_blocking(move || {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&lock_file)
                .map_err(|e| FileLockError::Open {
                    path: lock_file.to_string_lossy().to_string(),
                    source: e,
                })?;
            file.lock_exclusive().map_err(|e| FileLockError::Acquire {
                path: lock_file.to_string_lossy().to_string(),
                source: e,
            })?;
            Ok::<std::fs::File, FileLockError>(file)
        })
        .await
        .map_err(|e| FileLockError::Join(e.to_string()))?;
        let file = result?;
        Ok(FileLockGuard {
            _file: Arc::new(file),
        })
    }

    /// 非阻塞尝试：立即可得 → `Some(guard)`；被他人持有 → `Ok(None)`。
    pub async fn try_lock_exclusive(&self) -> Result<Option<FileLockGuard>, FileLockError> {
        let lock_file = self.lock_file.clone();
        let result = tokio::task::spawn_blocking(move || {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&lock_file)
                .map_err(|e| FileLockError::Open {
                    path: lock_file.to_string_lossy().to_string(),
                    source: e,
                })?;
            match file.try_lock_exclusive() {
                Ok(()) => Ok::<Option<std::fs::File>, FileLockError>(Some(file)),
                // WouldBlock = 被他人持有（非错误），返回 None。
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
                Err(e) => Err(FileLockError::Acquire {
                    path: lock_file.to_string_lossy().to_string(),
                    source: e,
                }),
            }
        })
        .await
        .map_err(|e| FileLockError::Join(e.to_string()))?;
        let file = result?;
        match file {
            Some(file) => Ok(Some(FileLockGuard {
                _file: Arc::new(file),
            })),
            None => Ok(None),
        }
    }
}

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        // fs2 unlock 是同步快速操作，直接调用（不必 spawn_blocking）。
        // Drop 不能返回错误，忽略 unlock 失败（进程退出时内核也会释放）。
        let _ = FileExt::unlock(self._file.as_ref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    /// try_lock_exclusive 首次 Some、再次 None（同路径自锁互斥）；guard Drop 后可再次获取。
    #[tokio::test]
    async fn test_try_lock_same_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("test.txt");
        let lock = FileLock::for_path(&path);

        let result1 = lock.try_lock_exclusive().await.expect("should not error");
        assert!(result1.is_some(), "first try_lock should succeed");

        let result2 = lock.try_lock_exclusive().await.expect("should not error");
        assert!(
            result2.is_none(),
            "second try_lock should fail (held by result1)"
        );

        drop(result1);

        let result3 = lock.try_lock_exclusive().await.expect("should not error");
        assert!(result3.is_some(), "try_lock after drop should succeed");
    }

    /// guard Drop 后锁可被再次获取（RAII 释放）。
    #[tokio::test]
    async fn test_guard_drop_releases() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("test.txt");
        let lock = FileLock::for_path(&path);

        {
            let _guard = lock.lock_exclusive().await.expect("should succeed");
        }

        let guard = lock
            .lock_exclusive()
            .await
            .expect("should succeed after drop");
        drop(guard);
    }

    /// 不同路径并行获取（互不干扰）。
    #[tokio::test]
    async fn test_different_paths_parallel() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path_a = dir.path().join("a.txt");
        let path_b = dir.path().join("b.txt");
        let lock_a = FileLock::for_path(&path_a);
        let lock_b = FileLock::for_path(&path_b);

        let guard_a = lock_a.lock_exclusive().await.expect("should succeed");
        let guard_b = lock_b.lock_exclusive().await.expect("should succeed");

        drop(guard_a);
        drop(guard_b);
    }

    /// lock_exclusive 可被外层 tokio::select! 取消（取消后返回 Cancelled，不永久阻塞）。
    #[tokio::test]
    async fn test_lock_exclusive_cancelled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("test.txt");
        let lock = FileLock::for_path(&path);

        // 后台任务先拿到锁并持有 500ms，用 AtomicBool 信号确认已持锁。
        let lock_holder = lock.clone();
        let acquired = Arc::new(AtomicBool::new(false));
        let acquired_clone = Arc::clone(&acquired);
        let holder = tokio::spawn(async move {
            let _guard = lock_holder.lock_exclusive().await.expect("should succeed");
            acquired_clone.store(true, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(500)).await;
        });

        // 等 holder 确实持锁，保证主任务的 lock_exclusive 必然进入等待。
        for _ in 0..200 {
            if acquired.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            acquired.load(Ordering::SeqCst),
            "holder should hold the lock"
        );

        // 50ms 后取消，select! 应走 Cancelled 分支。
        let signal = CancellationToken::new();
        let sig2 = signal.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            sig2.cancel();
        });

        let result = tokio::select! {
            g = lock.lock_exclusive() => g,
            _ = signal.cancelled() => Err(FileLockError::Cancelled),
        };

        assert!(
            matches!(result, Err(FileLockError::Cancelled)),
            "should be cancelled by outer select"
        );

        holder.await.expect("holder should complete");
    }

    /// FileLockError::Join 变体的 Display 实现。
    #[test]
    fn test_join_error_display() {
        let err = FileLockError::Join("test error".to_string());
        assert_eq!(err.to_string(), "join error: test error");
    }

    /// FileLockError::Open 变体的 Display 实现。
    #[test]
    fn test_open_error_display() {
        let err = FileLockError::Open {
            path: "/nonexistent/dir/file.guigu.lock".to_string(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "not found"),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("/nonexistent/dir/file.guigu.lock"),
            "should contain path"
        );
        assert!(msg.contains("not found"), "should contain source message");
    }
}
