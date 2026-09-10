# Task 028: 跨进程会话锁 / 多写者文件锁

## Background

006 交付的 `FileMutationQueue` 是**进程内** per-path 写锁；012 交付的 `SharedSessionStorage` 是**进程内**多 lane append 串行化。二者都只在单进程内保证并发安全，均已声明「跨进程多写者需文件锁层级，属后续任务」（006 边界、012 边界、009 边界）。

roadmap 候选 5 立项。本任务补上跨进程这一层：让多个 guigu 进程**写同一文件 / 写同一 session JSONL** 时串行化，避免互相覆盖与日志交错。

## Goal

- 新增跨进程文件锁原语 `FileLock`（基于 `fs2`，Unix flock / Windows LockFileEx），支持**阻塞 / 非阻塞 / 超时 / 可取消**，RAII guard，进程崩溃由内核自动释放
- 让 `FileMutationQueue` **可选**叠加跨进程锁（写工具跨进程串行化）
- 让 `JsonlSessionStorage` **可选**叠加跨进程锁（session append 跨进程串行化）
- **零破坏**：默认行为（不 opt-in 跨进程锁）与既有完全一致

## Design Notes

### 依赖与选型

```toml
[dependencies]
fs2 = "0.4"
```

- **选 `fs2` 而非 `fd-lock`**：fs2 成熟、跨平台（Unix `flock` + Windows `LockFileEx` 统一封装）；`fd-lock` 的 Windows 支持不完整。fs2 为同步阻塞 API，用 `tokio::task::spawn_blocking` 包裹，避免阻塞 async runtime 线程。
- **选 flock/LockFileEx（内核态锁）而非「锁文件 + PID」用户态方案**：进程崩溃（含 `kill -9`）后内核**自动释放**锁，不存在「遗留锁文件」需清理/判活的问题——这是正确处理「崩溃遗留锁」的核心手段（roadmap 风险项由此化解）。

### FileLock（src/core/file_lock.rs）

```rust
/// 跨进程独占锁：以锁文件路径标识互斥域，所有进程对同一锁文件路径互斥。
pub struct FileLock {
    lock_file: std::path::PathBuf,
}

pub struct FileLockGuard<'a> { /* 持有 fs2 锁句柄，Drop 自动解锁 */ }

impl FileLock {
    /// 构造锁（不立即加锁）。锁文件与目标文件同级目录、命名 `<目标文件名>.guigu.lock`。
    pub fn for_path(target: &Path) -> FileLock;

    /// 阻塞获取独占锁（spawn_blocking 包裹 fs2 锁调用，可被外层 select 取消/超时）。
    pub async fn lock_exclusive(&self) -> Result<FileLockGuard<'_>, FileLockError>;

    /// 非阻塞尝试：立即可得 → Some(guard)；被他人持有 → Ok(None)。
    pub async fn try_lock_exclusive(&self) -> Result<Option<FileLockGuard<'_>>, FileLockError>;
}

impl Drop for FileLockGuard<'_> { /* fs2 unlock */ }
```

**关键语义**：

1. **锁文件策略**：锁文件路径 = 目标文件 + 后缀 `.guigu.lock`（如 `a.txt.guigu.lock`、`session.jsonl.guigu.lock`）。多个进程写同一目标文件时，它们各自用同一锁文件路径 → 天然互斥。锁文件可被 fs2 自动创建（`OpenOptions::create(true)`），不参与业务数据读写。
2. **阻塞 + 可取消 + 可超时**：`lock_exclusive` 内部用 `spawn_blocking` 跑 `fs2::FileExt::lock_exclusive`（阻塞 syscall）；调用方用 `tokio::select!` 与 `signal.cancelled()` / `sleep(timeout)` 组合实现取消与超时。**本方法不内置超时**（对齐 006 `FileMutationQueue::acquire` 的「等待可被外层 select 打断」契约）。
3. **RAII + 崩溃释放**：`FileLockGuard` Drop 即 `fs2::unlock`，覆盖异常/取消提前返回；进程崩溃则内核释放 flock/LockFileEx，无需清理锁文件。
4. **spawn_blocking 细节**：锁操作（`lock_exclusive`/`try_lock`/`unlock`）均在 `spawn_blocking` 内执行；guard 持有的是**已加锁的 `File` 句柄**（`Arc<File>`），Drop 时的 `unlock` 是同步快速操作，直接调用即可，不必再 spawn_blocking。
5. **不跨 await 持 std 锁**：`FileLockGuard` 内部仅持 `File` 句柄 + 生命周期借用，无 std Mutex，天然满足「不持锁跨 await」。

### 错误语义（FileLockError）

```rust
#[derive(Debug, thiserror::Error)]
pub enum FileLockError {
    #[error("failed to open lock file `{path}`: {source}")] Open { path: String, #[source] source: std::io::Error },
    #[error("failed to acquire lock on `{path}`: {source}")] Acquire { path: String, #[source] source: std::io::Error },
    #[error("lock task cancelled")] Cancelled,
    #[error("join error: {0}")] Join(String),
}
```

- `Cancelled`：外层 select 在 spawn_blocking 完成前取消时返回；`Join`：spawn_blocking 任务 panic/join 失败。
- 锁**超时**由调用方表达（`tokio::select!` 超时分支返回调用方自己的超时错误），`FileLockError` 不设超时变体（避免语义重复）。

### 集成 1：FileMutationQueue 可选跨进程锁（src/tools/file_mutation_queue.rs）

```rust
impl FileMutationQueue {
    pub fn new() -> Self;                                            // 既有：仅进程内
    /// 新增：进程内 per-path 锁 + 跨进程锁双层。
    pub fn with_file_lock(lock: FileLock) -> Self;                   // 或接受 Arc<FileLock>
}
```

- `acquire(path)` 时序（在既有进程内 per-path 锁之上，**仅当配置了跨进程锁时**追加）：先进程内 per-path 锁（既有）→ 再 `file_lock.lock_exclusive()` 跨进程锁（新增）。跨进程锁在**拿到进程内锁之后、IO 之前**获取，避免「进程内已串行、跨进程还抢锁」的无效竞争。
- **零破坏**：`WriteTool::new(queue: Arc<FileMutationQueue>)` 签名不变；`FileMutationQueue::new()` 默认行为与 006 完全一致。嵌入方需要跨进程安全时改用 `with_file_lock`。
- 锁文件路径：跨进程锁的锁文件按**目标文件路径**确定（`FileLock::for_path(target)`），故每个 `acquire(path)` 用 `FileLock::for_path(path)` 动态构造（或内部缓存 lock 对象），不同路径互不干扰、同路径跨进程互斥。

### 集成 2：JsonlSessionStorage 可选跨进程 append（core/session/jsonl.rs）

```rust
impl JsonlSessionStorage {
    pub fn open(path) -> ...;                     // 既有：单进程
    /// 新增：append 跨进程串行化。
    pub fn open_locked(path, lock: FileLock) -> ...;  // 或 open_with(path, Option<FileLock>)
}
```

- append 时序：`file_lock.lock_exclusive().await` → 落盘 append → guard Drop 解锁。**仅包 append**，`load` 不抢锁（对齐 012「load 只在无活跃写时调用」的既有约定）。
- **零破坏**：`open` 默认无跨进程锁，行为与 009 一致。
- 锁文件路径：`<session.jsonl>.guigu.lock`，同一 session 文件的多个进程共享同一锁文件。

### 边界声明（明确不做）

- **锁粒度 = 整个文件**（独占锁）：不做字节范围锁（byte-range lock）、不做读写锁降级。
- **不做锁租约/心跳/自动过期**：flock 无 TTL；进程崩溃由内核兜底释放（见上），故无需租约。
- **NFS 网络文件系统不支持**：flock 在 NFS 上的语义历史不可靠，声明为不支持（本地文件系统 / 常规挂载可用）。
- **Windows 语义**：fs2 已封装 LockFileEx，语义声明支持；但 CI 测试环境假设 Linux，**测试仅保证 Unix**，Windows 以 fs2 文档为准（声明为已知边界）。
- **不做分布式锁 / 跨主机协调**（Redis/etcd）：本任务仅进程间文件锁，跨主机属其他机制，超出范围。

## Files

- Cargo.toml（`fs2` 依赖）
- src/core/file_lock.rs（`FileLock` + `FileLockGuard` + `FileLockError` + 单测）
- src/core/mod.rs（`pub mod file_lock`）
- src/lib.rs（re-export `FileLock`/`FileLockGuard`/`FileLockError`）
- src/tools/file_mutation_queue.rs（`with_file_lock` 可选叠加 + 单测）
- src/core/session/jsonl.rs（`open_locked` 可选叠加 + 单测）
- tests/file_lock.rs（跨进程锁集成测试：双进程互斥）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets -D warnings passes
- [ ] cargo test --all-targets passes
- [ ] cargo fmt --check passes
- [ ] `FileLock` 单测：`try_lock_exclusive` 首次 `Some`、再次 `None`（同路径自锁互斥）；guard Drop 后可再次获取；不同路径并行获取
- [ ] 双进程互斥集成测试：spawn 子进程（当前测试 binary 用 env 开关进入「持有锁 N 秒」分支），父进程 `try_lock_exclusive` 在子进程持有期间为 `None`，子进程退出后为 `Some`（证明跨进程生效，非仅线程内）
- [ ] `lock_exclusive` 可被外层 `tokio::select!` 取消（取消后返回 `Cancelled` 或等价，不永久阻塞）
- [ ] `FileMutationQueue::with_file_lock`：同路径跨进程串行（复用文件锁双进程测试）；`new()` 默认行为与 006 一致（既有测试全绿）
- [ ] `JsonlSessionStorage::open_locked`：跨进程 append 不交错半行（双进程各写 N 条 → 崩溃恢复/load 后条数正确、无半行）；`open()` 默认行为与 009 一致（既有测试全绿）
- [ ] 产品代码无 `unwrap()`；异步测试用 `tokio::test`；spawn_blocking 的 Join 错误路径有测试；tempdir 用 `tempfile`
- [ ] 单文件 ≤ 400 行，超则拆子模块并记录

## 修订记录

- v1.0（2026-09，Architect）：初稿。roadmap 候选 5 立项，十期第二项。`FileLock` 原语选 `fs2`（跨平台 flock/LockFileEx，内核态锁崩溃自释放，化解「遗留锁」风险），同步阻塞 API 用 `spawn_blocking` 包裹；`FileMutationQueue::with_file_lock` 与 `JsonlSessionStorage::open_locked` 可选叠加，零破坏既有 `new()`/`open()` 默认。锁粒度整个文件、无租约、NFS 不支持、Windows 仅声明不测试，均列为边界。
