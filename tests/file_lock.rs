//! Task 028：跨进程文件锁集成测试（双进程互斥）。
//!
//! 子进程（当前测试 binary 经 env 开关进入「持有锁 N 秒」分支）持有锁期间，
//! 父进程 `try_lock_exclusive` 为 `None`；子进程退出后为 `Some`。证明跨进程
//! 生效（非仅线程内）。
//!
//! 子进程模式通过 env `GUIGU_FILE_LOCK_CHILD` 触发，锁路径经 env
//! `GUIGU_FILE_LOCK_PATH` 传递（避免硬编码路径）。

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use guigu::{FileLock, FileMutationQueue};

/// 子进程模式开关 env。
const CHILD_MODE_ENV: &str = "GUIGU_FILE_LOCK_CHILD";
/// 锁目标路径 env（父进程传递，子进程读取）。
const LOCK_PATH_ENV: &str = "GUIGU_FILE_LOCK_PATH";
/// 子进程持锁时长（秒）。
const HOLD_SECS: u64 = 2;

/// 子进程模式：持有锁 `HOLD_SECS` 秒后释放。
///
/// 由父进程 spawn 当前测试 binary 并设 `CHILD_MODE_ENV` 触发。
async fn child_hold_lock() {
    let lock_path = std::env::var(LOCK_PATH_ENV).expect("lock path env should be set");
    let lock = FileLock::for_path(Path::new(&lock_path));
    let _guard = lock
        .lock_exclusive()
        .await
        .expect("child should acquire lock");
    // 持锁 N 秒（父进程在此期间验证互斥）。
    tokio::time::sleep(Duration::from_secs(HOLD_SECS)).await;
    // guard Drop 释放锁。
}

/// Task 028：双进程互斥集成测试。
///
/// 子进程持有锁期间，父进程 `try_lock_exclusive` 为 `None`；子进程退出后为
/// `Some`。证明跨进程生效（非仅线程内）。
#[tokio::test]
async fn test_cross_process_mutual_exclusion() {
    // 子进程模式：持有锁 N 秒后返回。
    if std::env::var(CHILD_MODE_ENV).is_ok() {
        child_hold_lock().await;
        return;
    }

    // 父进程模式：spawn 子进程并验证互斥。
    let dir = tempfile::tempdir().expect("tempdir");
    let lock_target = dir.path().join("test.txt");
    let lock_target_str = lock_target.to_string_lossy().to_string();

    // spawn 子进程（当前测试 binary，设 CHILD_MODE_ENV 进入子进程模式）。
    let exe = std::env::current_exe().expect("current exe");
    let mut child = Command::new(exe)
        .env(CHILD_MODE_ENV, "1")
        .env(LOCK_PATH_ENV, &lock_target_str)
        .arg("test_cross_process_mutual_exclusion")
        .arg("--exact")
        .spawn()
        .expect("spawn child");

    // 等子进程确实持锁（轮询锁文件存在 + 短暂等待确保 lock_exclusive 完成）。
    // 锁文件由 fs2 自动创建（OpenOptions::create(true)），子进程持锁期间锁文件存在。
    // 但锁文件在子进程启动时即创建（lock_exclusive 前），故用「父进程 try_lock 失败」
    // 作为子进程已持锁的信号。
    let lock = FileLock::for_path(&lock_target);
    let mut child_holding = false;
    for _ in 0..100 {
        match lock.try_lock_exclusive().await {
            Ok(None) => {
                // 被他人持有 = 子进程已持锁。
                child_holding = true;
                break;
            }
            Ok(Some(_guard)) => {
                // 子进程尚未持锁，释放后重试。
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
    assert!(child_holding, "child should hold the lock within timeout");

    // 子进程持锁期间，父进程 try_lock 应为 None（跨进程互斥生效）。
    let result = lock.try_lock_exclusive().await.expect("should not error");
    assert!(
        result.is_none(),
        "try_lock should fail while child holds the lock (cross-process)"
    );

    // 等子进程退出（持锁 N 秒后释放）。
    let status = child.wait().expect("wait for child");
    assert!(status.success(), "child should exit successfully");

    // 子进程退出后，父进程 try_lock 应为 Some（锁已释放）。
    let result = lock.try_lock_exclusive().await.expect("should not error");
    assert!(
        result.is_some(),
        "try_lock should succeed after child exits (lock released)"
    );
}

/// Task 028：FileMutationQueue::with_file_lock 同路径跨进程串行。
///
/// 子进程持有锁期间，父进程 `FileMutationQueue::with_file_lock().acquire(path)`
/// 阻塞（跨进程锁被持有）；子进程退出后 `acquire` 成功。
#[tokio::test]
async fn test_cross_process_file_mutation_queue() {
    // 子进程模式：持有锁 N 秒后返回。
    if std::env::var(CHILD_MODE_ENV).is_ok() {
        child_hold_lock().await;
        return;
    }

    // 父进程模式：spawn 子进程并验证 FileMutationQueue 跨进程串行。
    let dir = tempfile::tempdir().expect("tempdir");
    let lock_target = dir.path().join("queue.txt");
    let lock_target_str = lock_target.to_string_lossy().to_string();

    // spawn 子进程（当前测试 binary，设 CHILD_MODE_ENV 进入子进程模式）。
    let exe = std::env::current_exe().expect("current exe");
    let mut child = Command::new(exe)
        .env(CHILD_MODE_ENV, "1")
        .env(LOCK_PATH_ENV, &lock_target_str)
        .arg("test_cross_process_file_mutation_queue")
        .arg("--exact")
        .spawn()
        .expect("spawn child");

    // 等子进程确实持锁（父进程 try_lock 失败 = 子进程已持锁）。
    let probe_lock = FileLock::for_path(&lock_target);
    let mut child_holding = false;
    for _ in 0..100 {
        match probe_lock.try_lock_exclusive().await {
            Ok(None) => {
                child_holding = true;
                break;
            }
            Ok(Some(_guard)) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
    assert!(child_holding, "child should hold the lock within timeout");

    // 子进程持锁期间，父进程 FileMutationQueue::with_file_lock().acquire 应阻塞。
    // 用 select! + 超时验证：acquire 应在超时前被取消（跨进程锁被持有）。
    let queue = FileMutationQueue::with_file_lock();
    let path = lock_target.clone();
    let cancelled = tokio::select! {
        _guard = queue.acquire(&path) => false, // acquire 成功 = 跨进程锁未生效（不应发生）
        _ = tokio::time::sleep(Duration::from_secs(1)) => true, // 超时 = acquire 阻塞（跨进程锁生效）
    };
    assert!(
        cancelled,
        "acquire should block while child holds the lock (cross-process serialization)"
    );

    // 等子进程退出（持锁 N 秒后释放）。
    let status = child.wait().expect("wait for child");
    assert!(status.success(), "child should exit successfully");

    // 子进程退出后，父进程 FileMutationQueue::with_file_lock().acquire 应成功。
    let _guard = queue.acquire(&path).await;
    // guard Drop 释放锁。
}
