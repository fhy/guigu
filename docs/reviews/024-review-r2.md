# Task 024 Review - Round 2

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

1. **[Critical] 初始 head 持久化与 bridge 写入存在反向覆盖竞态**
   - 位置：`src/server/lane.rs:98-135`、`src/server/lane.rs:183-220`
   - 影响：`spawn_lane_with`/`fork_lane` 在第 4/5 步先启动 `spawn_bridge` 并登记 lane，第 6/7 步才调用 `persist_head`。登记完成后，bridge 已可消费 `MessageEnd`：它可能先通过 `append_with_head` 成功写入消息及新 head `H1`，随后创建流程才执行 `persist_head`，再次追加初始 head `H0`。append-only 重放取最后一条记录，因此恢复时得到 `H0`，丢失该 lane 已成功写入的 `H1`，造成持久化 head 回退。fork 场景尤其容易把新分支的首条消息覆盖回分叉点。
   - 建议：不要在 bridge 已可写入后再无条件追加初始 head。可在登记前完成“初始 head + writer/bridge 可写”所需的原子预留，或让初始 head 与首次写入通过 session 级串行提交/状态机完成；至少在持有 writer 锁时检查当前 head，只在仍等于初始 head 时写入，并确保该检查与追加初始记录之间不会被 bridge 的 append 插入。补充“spawn/fork 后立即并发 append，最终恢复 head 必为最后一次 append”回归测试。

### 已验证修复

- `SharedSessionStorage::append_with_head` 已将 message 与 head 追加纳入同一写锁。
- `LaneWriter::append` 仅在组合提交成功后推进内存 head。
- 恢复路径使用 `snapshot` 获取一致的 tree/head 视图。
- 旧 `StorageFactory` 签名已恢复，并保留 bundle factory 扩展。
- 测试已覆盖 head 写失败、空 lane/fork 恢复、并发 append、重复 spawn/fork 竞态及旧 API 兼容。

## 结论

- [ ] 通过
- [x] 打回

## 下一步

- @guigu-worker 请修复上述 Critical 问题，重点保证 lane 登记、初始 head 和 bridge 首次写入之间不存在 head 回退窗口，并补充并发回归测试。
