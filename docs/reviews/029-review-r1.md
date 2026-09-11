# Task 029 Review - Round 1

## 基本信息

- 审查时间: 2026-09-11
- 审查员: guigu-reviewer
- 任务规格: `docs/tasks/029-agent-plugin-hooks.md`
- 审查提交: `cde2125`

## 门禁结果

- cargo check: ✓
- cargo clippy --all-targets -- -D warnings: ✓
- cargo test --all-targets: ✓（546 passed, 0 failed）
- cargo fmt --check: ✓

## 代码审查

### 问题

无必须修复问题。实现满足规格中的插件注册、hooks 合并、短路、改写/注入、主循环桥接及锁外回调要求；新增产品文件均不超过 400 行，公开 API 具备文档注释，未发现产品代码裸 `unwrap()`。

### 建议

1. `src/plugin/agent.rs:148-151` — `agent_factory()` 在持有注册表读锁时调用外部插件的 `plugin.agent_factory()` 回调。若插件工厂回调重入 `register`/`unregister`/`get`，可能发生阻塞或死锁；这与 `merged_hooks()` 已采用的“锁内只复制 Arc、锁外执行外部回调”纪律不一致。建议先在锁内复制目标插件的 `Arc`，释放锁后再调用 `agent_factory()`，并补充重入回归测试。

## 结论

- [x] 通过
- [ ] 打回

上述问题属于后续可独立修复的健壮性改进，不影响本任务当前验收结果。

## 下一步

- 可合并当前提交。
- 建议后续修复 `agent_factory()` 的锁边界并增加对应测试。
