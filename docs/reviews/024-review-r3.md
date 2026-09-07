# Task 024 Review - Round 3

## 基本信息

- 审查时间: 2026-09-07
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/024-lane-head-persistence.md
- 审查提交: 0dd9944

## 门禁结果

- cargo check: ✓
- cargo clippy --all-targets -- -D warnings: ✓
- cargo test --all-targets: ✓（288 库测试、10 个 binary 测试及全部集成测试通过）
- cargo fmt --check: ✓

## 代码审查

### 问题

1. **[Critical] 初始 head 持久化仍可能覆盖 bridge 已写入的新 head**
   - 位置：`src/server/lane.rs:98-135`、`src/server/lane.rs:183-220`
   - 影响：`spawn_bridge` 在登记前启动，登记后即可处理 `MessageEnd`。若 bridge 先通过 `append_with_head` 写入消息及 head `H1`，随后创建流程才执行 `persist_head`，就会追加初始 head `H0`。JSONL 重放按最后写入值覆盖，恢复得到 `H0`，丢失已成功写入的 `H1`；fork 场景会把新 lane 回退到分叉点。
   - 建议：将 lane 登记、初始 head 记录和 bridge 开始写入纳入同一 session 级串行提交/状态机；或在 writer 的同一写锁保护下执行“仅当当前 head 仍为初始值才追加”的 compare-and-append，并保证检查与追加不可被 bridge 插入。补充“登记后立即并发 append，最终恢复 head 必须等于最后一次 append”测试。

2. **[High] `persist_head` 本身未使用 SharedSessionStorage 写锁，无法作为上述竞态的安全修复基础**
   - 位置：`src/core/session.rs:372-391`、`src/core/session/lane_writer.rs:62-70`
   - 影响：`LaneWriter::persist_head` 通过 `SharedSessionStorage` 的 `LaneHeadStore` 实现直接委托底层 store；该路径没有获取 `write_lock`，因此即使调整调用时机，仍可能与 `append_with_head` 的组合提交交错。当前写锁只覆盖 `append`/`append_with_head`，不能保证 head 初始记录与 message/head 提交的顺序。
   - 建议：提供由 `SharedSessionStorage` 统一加写锁的持久化入口，并在其中实现初始化 head 的条件提交或完整 lane 创建提交；不要让公开的 `LaneHeadStore` 委托路径绕过共享写锁。

3. **[High] 持久化失败回滚可能删除同名的新 lane**
   - 位置：`src/server/lane.rs:128-135`、`src/server/lane.rs:215-220`、`src/server/lane.rs:233-242`
   - 影响：初始 head 持久化失败后，代码先释放当前 lane 的登记锁，再调用 `remove_lane(session_id, lane_id)`。若期间发生 shutdown/清理并由另一个请求成功重新登记同名 lane，`remove_lane` 没有校验 `LaneRuntime` 身份，会误删新请求创建的 lane。
   - 建议：回滚时按预留 token/`Arc` 身份或 generation 删除，仅删除本次插入的 `LaneRuntime`；更根本地，应在 session 锁内完成 lane 预留和创建提交，失败时原子撤销本次预留。

## 已验证修复

- message 与自动 head 追加已通过 `append_with_head` 置于同一写锁。
- append 失败时内存 head 不推进。
- 恢复入口已使用一致性 snapshot。
- 旧 StorageFactory API 已恢复，并提供 bundle factory 扩展。
- 已补充 head 写失败、空 lane/fork 恢复、并发 append、竞态与旧 API 测试；但尚未覆盖初始 head 与 bridge 首次写入的反向覆盖窗口。

## 结论

- [ ] 通过
- [x] 打回

## 下一步

- @guigu-worker 请修复上述问题 1-3，优先解决初始 head 与 bridge 写入的顺序/原子性；补充可稳定复现反向覆盖的并发回归测试，并重新运行四门禁。
