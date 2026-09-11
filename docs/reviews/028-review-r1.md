# Task 028 Review - Round 1

## 基本信息
- 审查时间: 2026-09-11
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/028-cross-process-lock.md
- 审查提交: 9b8e64c

## 门禁结果
- cargo check: ✓
- cargo clippy -- -D warnings: ✓
- cargo test: ✓（338 个单元测试；集成测试通过）
- cargo fmt --check: ✓

## 代码审查
### 问题
1. **[Critical] src/core/session/jsonl.rs:137-158** — 跨进程锁获取发生在 `next_id` 认领之后，且 `next_id` 仅为进程内 `AtomicU64`。
   - 影响：两个进程分别 `open_locked` 同一个已有/空 session 时都会从相同游标开始；进程 A 和 B 可分别认领 id `0`，随后在文件锁下依次写入两条重复 id。`load` 最终会触发重复 ID 错误或丢失一条消息，未满足“跨进程 append 正确”的核心目标。
   - 建议：先获取文件锁，再在锁保护范围内重新读取/更新文件中的最大消息 ID（或设计跨进程共享的 ID 分配机制），然后分配 ID 并完成写入；锁必须覆盖“读取游标/分配 ID/写入/sync_all”整个事务。补充至少两个独立 `JsonlSessionStorage` 实例/进程并发 append 的测试，断言 ID 唯一、load 条数完整。

2. **[High] src/tools/file_mutation_queue.rs:97-110** — 跨进程锁获取失败时记录错误后继续返回 guard。
   - 影响：启用 `with_file_lock()` 后，`lock_exclusive()` 的打开失败、权限错误或 join 错误都会被吞掉，调用方仍会执行写 IO；此时实现退化为进程内锁，直接违反该 opt-in 模式的跨进程串行保证，可能造成多进程写覆盖/交错。错误日志不足以恢复安全语义。
   - 建议：不要静默继续。由于当前 `acquire` 签名不能返回 `Result`，应先与规格/调用方确认并改为 `Result<FileMutationGuard<'_>, FileMutationError>`，或增加 `try_acquire`/可配置错误策略；至少在无法取得跨进程锁时拒绝进入写临界区。相应更新 `WriteTool` 调用链和测试，覆盖锁文件父目录不可写等失败路径。

3. **[Warning] src/core/session/jsonl.rs:94-102** — `FileLockError` 被转换为 `SessionError::Io(std::io::Error::other(e.to_string()))`，丢失错误类型和 source 链。
   - 影响：调用方无法区分锁打开失败、锁获取失败、join 失败或取消；也无法通过 `source()` 保留原始 IO 错误，降低诊断能力。尤其在锁获取失败时，错误语义与普通 session IO 错误混在一起。
   - 建议：在 `SessionError` 中增加带 `#[source]` 的文件锁错误变体（或为其提供专用错误类型），直接 `map_err(SessionError::FileLock)` 传播 `FileLockError`。

### 规格偏差/建议
1. `src/tools/file_mutation_queue.rs:157-162` 已将 `FileLockGuard<'a>` 改成拥有句柄的 guard，生命周期设计本身合理；但应同步修订任务规格/API 文档，避免公开 API 文档仍声明生命周期参数。
2. `src/tools/file_mutation_queue.rs` 为 463 行，超过 conventions 的 400 行限制。开发者已记录但未拆分；后续应将测试或跨进程集成逻辑拆到子模块，避免继续增长。
3. 规格要求的“JsonlSessionStorage 双进程各写 N 条，恢复后条数正确、无半行”测试尚未覆盖；现有 `open_locked` 测试均为单实例顺序 append，不能验证上述 Critical 问题。

## 结论
- [ ] 通过
- [x] 打回

## 下一步
- @guigu-worker 请优先修复问题 1、2；问题 3 建议一并修复。
- 修复后重新运行 `cargo check`、`cargo clippy -- -D warnings`、`cargo test`、`cargo fmt --check`，并提交新的审查轮次。
