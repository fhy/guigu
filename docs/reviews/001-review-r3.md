# Task 001 Review R3: Agent trait + 生命周期（AgentHandle）

- 日期: 2026-08-22
- 结论: **REJECT**（与 R2 相同的三个核心问题未修复）
- 范围: e520cae（R2 打回 0ffebd0 之后 src/ tests/ 无任何改动）

## DoD 门禁

| # | 门禁 | 结果 |
|---|------|------|
| ① | cargo check | ✓（1 warning：dead_code） |
| ② | cargo clippy --all-targets -D warnings | ✗ FAIL（dead_code: `AgentHandle.tx`、`AgentHandle.idle` 未读） |
| ③ | cargo test | 编译通过全绿，**但 agent_lifecycle 5 个测试 + echo_agent 1 个测试均为 `assert!(true)` 假测试** |
| ④ | cargo fmt --check | ✗ FAIL（agent.rs 多处格式差异） |

## 问题清单

### 阻断项（与 R2 相同，必须修复）

1. **src/core/agent.rs:41 — `spawn` 仍是 `todo!`**
   规格要求 `spawn` 启动唯一 runtime task（Handle → mpsc → actor → watch/broadcast 全通路）。当前调用即 panic，整个 actor 外壳不存在 → 按 Design Notes 实现 mpsc 消费循环。

2. **src/core/agent.rs:64-73 — `Agent` trait 非 async，无 `#[async_trait]`**
   规格：`prompt`/`continue_`/`steer`/`follow_up`/`wait_for_idle` 必须是 async 方法。当前全部为同步签名；`async-trait` 已在 Cargo.toml 却未使用 → 改为 `#[async_trait] pub trait Agent`。

3. **tests/agent_lifecycle.rs:4-31 — fake green**
   5 个测试全部只有 `assert!(true)`，注释自称"简化版本"。违反 conventions.md Test Quality 与任务 AC 第 5、6 条。AC 要求覆盖：prompt 后 snapshot.messages 增长、subscribe 完整事件序列、steer/followUp 入队 drain、abort 状态一致、wait_for_idle 结算、reset 清空 → 用 `#[tokio::test]` 对真实行为断言。

### 阻断项（新发现）

4. **src/core/agent.rs:52-60 — `wait_for_idle` / `shutdown` 为空操作直接返回 `Ok(())`**
   规格：shutdown 发 Shutdown 并等 task 退出；wait_for_idle 在 AgentEnd 结算后返回。当前语义完全缺失。

5. **tests/echo_agent.rs（未跟踪文件）— 又一个 `assert!(true)`**
   不在任务 001 文件清单内，且为假测试。要么删除，要么移出本任务范围。

### 重要问题

6. **src/core/agent.rs:132-273 — `InMemoryAgent` 未实现 `Agent` trait，也未走命令队列**
   只有固有方法，无 `impl Agent for ...`；规格要求 `impl Agent for AgentHandle { /* 转发到 tx */ }`。
7. **src/core/agent.rs:83 — 错误类型不符合规范**
   conventions 要求 thiserror；规格写 `AgentError::Busy` 枚举变体。当前为 struct + kind 字段 → 用 thiserror 定义 `enum AgentError { Busy, ... }`。
8. **公开 API 全部缺少 `///` 文档注释**（conventions Coding Standards）。
9. **cargo fmt 未跑**：提交前未过门禁④。

## 备注

- `model: Option<String>` vs 规格的 `ModelId` 属可接受的偏差（003 引入 ModelId 时再对齐），但请在代码注释注明。
- 依赖（async-trait、thiserror）已就位，无需改 Cargo.toml。
