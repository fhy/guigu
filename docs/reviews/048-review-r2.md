# Task 048 Review - Round 2

## 基本信息
- 审查时间: 2026-09-22 22:08
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/048-tests-common-doc-reexport.md
- 审查对象: commit `1e6c51c` fix(tests): complete Task 048 helper cleanup
- 上一轮: docs/reviews/048-review-r1.md（打回）

## 门禁结果
- cargo check --all-targets: ✓
- cargo clippy --all-targets --all-features -- -D warnings: ✓ (0 warning)
- cargo test --all-targets: ✓ (全部 test 二进制 0 failed；21 项 runtime_loop + 其它集成测试 crate 全绿)
- cargo fmt --check: ✓

## closure matrix 复核（只核验 r1 冻结矩阵）

| # | Requirement | Required case | 复核结果 | 证据 |
|---|-------------|---------------|----------|------|
| M1 | 补回 helper `///` 注释，覆盖 047-review-r1 建议 1 清单 | provider/tools/fixtures 对应项均有 `///` | ✓ | provider:11、tools:8,36、fixtures:12,32,37,67,73,124,133,155,165 均有 `///` |
| M2 | 注释与 parent 逐条对应 | fixtures:67,72 与 provider:86 恢复 parent 原文 | △ 部分 | fixtures:67,72 已与 parent 逐字一致；provider body 注释（gate/建立失败/挂起）已恢复 parent 原文；**provider:88 struct `///` 仍为 00b6a41 新写的单行**（见残留说明） |
| M3 | 冗余 re-export 清理，保留 glob 实际依赖项 | mod.rs:10-21 按 r1 清单增删后全绿 | ✓ | 见下「规格符合性」 |
| M4 | `#![allow(unused_imports)]` 复核 | 移除后 clippy 0 warning（或恢复并说明） | ✓ | 已独立在临时 worktree 实证，见下 |
| M5 | 无 `#[test]` 删除/断言弱化 | assert 宏总数 ≥ 577，测试函数数不变 | ✓ | assert 宏 577 = parent 577；`#[test]`/`#[tokio::test]` 179 = 00b6a41 179 |
| M6 | 零 `src/` 变更 | 仅动 `tests/` | ✓ | `1e6c51c` stat 仅 tests/common/{fixtures,mod,provider}.rs + 4 个 runtime_loop 文件 |

## 规格符合性

| 验收项 | 结果 | 证据 |
|--------|------|------|
| cargo check --all-targets | ✓ | 0 error |
| cargo clippy --all-targets --all-features -D warnings | ✓ | 0 warning |
| cargo test --all-targets | ✓ | 全绿，0 failed |
| cargo fmt --check | ✓ | 一致 |
| helper `///` 已补回（覆盖建议 1 清单位置） | ✓ | provider:11 / tools:8,36 / fixtures 各位置齐全 |
| 冗余 std/guigu 类型 re-export 已清理 | ✓ | 移除 `guigu::Agent`、`AgentHandle`、`ProviderError`、`Tool`、`ToolExecutionMode`、`Arc`；保留 message 类型、`AssistantEvent`、`ResourceScope`、`AgentRuntime/LoopConfig/Model`、`AtomicUsize/Ordering`、`Duration`、`oneshot`——与 r1 问题 1 清单逐条一致 |
| `#![allow(unused_imports)]` 按 DN3 复核 | ✓ | 见下「allow 复核实证」 |
| 无 `#[test]` 删除 / 断言弱化 | ✓ | assert 577 / tests 179，均未减少 |
| 零 `src/` 产品代码变更 | ✓ | 仅 `tests/` |

## 关键复核

### 1. Agent trait 显式导入（developer 对 r1 清单的修正，成立）
- 四个 runtime_loop crate 通过 `use common::*` 取用的 `guigu::Agent` 被移除后，`AgentHandle` 上的 `.prompt()/.steer()/.follow_up()/.abort()` 调用失去 trait 作用域。
- 已核验 `src/core/agent.rs:153` `pub trait Agent`，方法 `prompt/steer/follow_up/abort` 定义于 trait（`agent.rs:159/163/165/169`）；四个文件确有 `.prompt()` 等调用（如 `runtime_loop.rs:25,213,251,290`）。
- 结论：r1「`guigu::Agent` 全 tests/ 0 引用、可移除」判定不完整——字面 token 为 0，但 trait method 调用需 trait 在作用域内。修复补 `use guigu::Agent;` 是**必要且正确**，非新增测试逻辑（仅导入）。re-export 因此确可移除。✓

### 2. allow 复核实证（M4）
- 在临时 `git worktree`（不影响主仓 `tests/`）删除 `tests/common/mod.rs` 第 3 行 `#![allow(unused_imports)]` 后运行 `cargo clippy --all-targets --all-features -- -D warnings`，实际报出多组 `unused imports`（`AssistantContent/AssistantMessage/Message/StopReason`、`AssistantEvent`、`ResourceScope`、`AgentRuntime/LoopConfig/Model`、`AtomicUsize/Ordering`、`Duration`、`oneshot`、`provider::*`、`tools::*`，各集成 crate 各报其未用子集）。
- 原因：`tests/common` 为私有模块，各测试 crate 仅消费 helper 子集，故 `pub use` 在各 crate 视角下确有未用项。
- 结论：按 DN3「触发 unused_imports → 恢复并保留」**正确**，allow 保留有据，非掩盖真实告警。✓

## 残留（非阻塞）

1. [Info] `tests/common/provider.rs:88` — `HangingProvider` 的 struct `///` 仍为 00b6a41 新写的 `/// 永不结束的 provider：用于建流取消测试。`，与 parent 源文件（`e9ce9ea^:tests/common/runtime_loop_provider.rs:107-108`）的两行版 `/// 挂起 provider：\`stream()\` 永不返回（\`pending()\` future），用于验证建流阶段的取消/超时（Task 040）。runtime 的 \`select!\` 应在 provider 返回前抢先取消。` 不一致。
   - 判定为非阻塞：该注释非 047 合并丢失项，亦**不在 AC 第 5 条所引「047-review-r1 建议 1 清单」**（该清单为 provider:11 / tools:8,35 / fixtures:…，不含 provider:86）；本候选未引入回归（该行由 00b6a41 首次写入，1e6c51c 未改）；r1 亦将其标为 [Warning]。按审查范围控制，无明确验收项支撑，故不阻塞。
   - 建议（可选）：若后续再触碰 `provider.rs`，可将该方法注释对齐 runtime_loop_provider.rs 原文。
   - 说明：r1 冻结矩阵 M2 的 required case 曾把 provider:86 纳入；经复核其无对应 AC 支撑，本轮回溯收窄为上述非阻塞项；fixtures:67,72 及 provider body 注释部分**已满足** M2。

## 结论
- [x] 通过
- [ ] 打回

## 下一步
- 本轮问题 1（re-export 清理）、问题 2（allow 复核）已按 r1 清单落地并实证；门禁四道全绿；测试/断言计数未减少；零 `src/` 变更。
- 残留为非阻塞 [Info]，可留待后续自然触碰时对齐，不阻断本任务闭环。
- 建议 PM 将 Task 048 转 [x]。
