# Task 030: Agent 插件 agent_factory 锁外回调纪律

## Background
029 r1 非阻塞建议 1（docs/reviews/029-review-r1.md）：`src/plugin/agent.rs:148-151` 的 `agent_factory()` 在持有 `AgentPluginRegistry` 读锁期间调用外部插件 `plugin.agent_factory()` 回调。若回调重入 `register`/`unregister`/`get`，可能阻塞或死锁；与同文件 `merged_hooks()` 已采用的「锁内只复制 Arc、锁外执行外部回调」纪律不一致。

## Goal
修复 `agent_factory()` 锁边界：读锁内仅「查找 + 复制 `Arc<dyn AgentPlugin>`」，释放锁后再调用 `agent_factory()`；补充重入回归测试。

## Design Notes
- 锁纪律：读锁作用域内只做查找与 `Arc` 复制，不得调用任何外部插件方法。
- 释放锁后调用 `plugin.agent_factory()`，返回类型以实际代码为权威（`Option<Arc<dyn AgentFactory>>` 或既有等价形态）。
- 不改公开 API 签名，`AgentPluginRegistry::agent_factory` 对外行为不变。
- 实现模式与同文件 `merged_hooks()` 保持一致（参考其既有锁外回调写法）。

## Files
- src/plugin/agent.rs（回调移出读锁）

## 错误处理
无新错误类型；回调返回类型保持既有契约。

## 测试要求
- 重入回归测试：构造一个插件，其 `agent_factory()` 回调内再次调用 registry 的 `register`/`unregister`/`get`，断言调用 `registry.agent_factory(id)` 不死锁且返回正确结果。
- 既有 029 插件相关测试保持通过。

## Acceptance Criteria
- [ ] cargo check
- [ ] cargo clippy --all-targets -- -D warnings
- [ ] cargo test --all-targets
- [ ] cargo fmt --check
