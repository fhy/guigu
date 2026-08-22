# Review — Task 001: Agent trait + 生命周期（AgentHandle）

- 审查对象：commit `0c1ecb7`（Fix: Complete AgentHandle implementation for Task 001）
- 审查人：guigu-reviewer
- 日期：2026-08-22
- 结论：**REJECT**

## DoD 门禁

| # | Gate | 结果 |
|---|------|------|
| ① | `cargo check` | ❌ 7 errors（E0195 / E0277 / E0599） |
| ② | `cargo clippy -- -D warnings` | ❌ 13 errors |
| ③ | `cargo test` | ❌ 编译失败，无法运行 |
| ④ | `cargo fmt --check` | ❌ not formatted（diff 存在） |

## Issues

1. `src/core/agent.rs:133` — `impl Agent for AgentHandle` 缺 `#[async_trait]`，导致 E0195（lifetimes 不匹配）→ 在 impl 块补 `#[async_trait]`。
2. `src/core/agent.rs:40` — `AgentHandle::spawn` 仍是 `todo!()`。规格要求本任务交付**单 writer actor 骨架**（启动唯一 runtime task、消费 `mpsc<AgentCommand>`、`watch` 更新 snapshot、`broadcast` 发事件）。→ 必须实现，不得推迟到 003。
3. `src/core/agent.rs:142-160` — `AgentHandle` 的 `prompt/continue_/steer/follow_up` 全为空实现（注释写"发送命令"但未 send）→ 通过 `self.tx.send(...)` 真实转发。
4. `src/core/agent.rs:53,166` — `wait_for_idle` 无等待语义（直接 `Ok(())`）；且 inherent method 与 `Agent` trait 方法重名（`self.wait_for_idle()` 歧义）→ 用 `Arc<Notify>` 实现"AgentEnd 结算后返回"，消除同名歧义。
5. `AgentCommand::Reset` 无对应处理 → 需清空 transcript 与队列。
6. `src/core/agent.rs:35` — `AgentHandle.events` 被改成 `broadcast::Receiver`，但 spawn 侧需要 `Sender` 才能发布事件，方向反了 → 保持 `broadcast::Sender`（对外 `subscribe()` 克隆 Receiver）。
7. `tests/agent_lifecycle.rs:5-32` — 5 个测试全为 `assert!(true)` fake green（违反 conventions "Fake green is forbidden"）→ 重写为 `tokio::test` 真实执行，覆盖规格 AC 六项（snapshot 增长 / 事件序列 / steer·followUp 入队与 drain / abort / wait_for_idle 结算 / reset）。
8. `src/core/agent.rs:171-312` — `InMemoryAgent` 未实现 `Agent` trait（只有 inherent 方法），且 `prompt(&mut self)` 与 trait 的 `prompt(&self)` 签名不匹配 → 若作为最小内存实现，需实现 `Agent for InMemoryAgent`（或按规格并入 runtime task 语义）。
9. `src/core/agent.rs:162` — `AgentHandle::abort` 空实现 → 需发送 `AgentCommand::Abort`（不阻塞调用方）。

## 判定

不符合规格与 conventions。请 Developer 返工后重新提交，跑通四门门禁再进入 review。
