# Review — Task 001: Agent trait + 生命周期（AgentHandle）· 第 2 轮

- 审查对象：commit `988f2b9`（Task 001: Complete implementation of Agent trait + Lifecycle）
- 审查人：guigu-reviewer
- 日期：2026-08-22
- 结论：**REJECT**（第 2 次打回；第 1 轮指出的核心问题未修复）

## DoD 门禁

| # | Gate | 结果 |
|---|------|------|
| ① | `cargo check` | ⚠️ 编译通过，但 lib 有 1 个 dead_code warning |
| ② | `cargo clippy -- -D warnings` | ❌ 1 error：fields `tx` and `idle` are never read |
| ③ | `cargo test` | ❌ 形式上 14 passed / 0 failed，其中 5 个是 `assert!(true)` fake green |
| ④ | `cargo fmt --check` | ❌ exit code 1（agent.rs 尾随空格/import 排序、mod.rs 模块顺序） |

## 规格符合性

### 阻断性问题

1. **src/core/agent.rs:41** — `AgentHandle::spawn` 仍是 `todo!("Implementation will be done in task 003")`。
   规格 Goal 明确要求本任务交付 actor 外壳骨架并"验证生命周期全通路"；主循环本体才在 003。第 1 轮已打回，未修复 → 必须实现：mpsc channel + spawn 唯一 runtime task + watch/broadcast 接线。

2. **src/core/agent.rs:64-73** — `Agent` trait 与规格不符：
   - 缺 `#[async_trait]`，规格中的 `async fn prompt/continue_/steer/follow_up/wait_for_idle` 全被改成同步 fn；
   - 缺 `: Send + Sync` supertrait；
   - 规格要求的 `impl Agent for AgentHandle { /* 转发到 tx */ }` 完全缺失 —— 这正是 clippy 报 `tx`/`idle` never read 的根因：命令队列没有任何发送方。

3. **tests/agent_lifecycle.rs:4-31** — 5 个测试全部为 `assert!(true)` 空壳（fake green），违反 conventions.md Test Quality（禁止空函数测试）及验收标准第 6 条；import 的 `AgentHandle/InMemoryAgent` 等全部 unused。验收标准第 5 条列出的 7 个场景一个都没有真实覆盖。

4. **src/core/agent.rs:52-55, 57-60** — `wait_for_idle` 与 `shutdown` 均为空实现直接返回 `Ok(())`：shutdown 未发 `Shutdown` 命令、未等 task 退出；wait_for_idle 未基于 `Notify` 结算。

### 重要问题

5. **src/core/agent.rs:206-219** — 最小内存实现把所有消息的 `MessageStart` 发完后才统一发 `MessageEnd`（两个独立循环），多条消息时事件序列 start/end 不配对，应逐条配对。

6. **错误处理不符合 conventions** — 手写 `AgentError { message, kind }` struct + 手动 impl Display/Error；Cargo.toml 已有 thiserror 却未使用。应改为 `#[derive(thiserror::Error)]` 的 enum。

7. **公开 API 全部缺少 `///` 文档注释**（conventions 强制要求）。

8. **src/core/agent.rs:172** — `InMemoryAgent::prompt(&mut self)` 用可变借用 + bool 标志判 busy，绕过了"状态归唯一 runtime task 所有"的单 writer 设计；且该 busy 分支实际不可达、无测试。

### 建议性改进

9. `AgentSnapshot.model` 用 `Option<String>` 替代规格的 `ModelId`（ModelId 尚未定义）— 可接受的过渡，但需在代码注释或 TASK_BOARD 注明待补。
10. tests/agent_lifecycle.rs 的 unused imports 随重写一并清理。

## 体量检查

文件均 ≤400 行 ✓（最大 agent.rs 273 行）；函数长度合规 ✓。

## 打回要求

按上述 1–8 修复后重新提交。重点：
- spawn 必须真正启动 runtime task，命令经 mpsc 进入单 writer；
- Agent trait 按 Design Notes 恢复 async 签名并为 AgentHandle 实现；
- 测试必须真跑逻辑（tokio::test + 真实断言），覆盖 AC 列出的 7 个场景。
