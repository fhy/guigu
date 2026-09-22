# Task 048 Review - Round 1

## 基本信息
- 审查时间: 2026-09-22 21:58
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/048-tests-common-doc-reexport.md
- 审查对象: commit `00b6a41` test(common): restore helper docs and trim reexports

## 门禁结果
- cargo check --all-targets: ✓
- cargo clippy --all-targets --all-features -- -D warnings: ✓ (0 warning)
- cargo test --all-targets: ✓ (全绿；21 项 runtime_loop + 其它集成测试 crate 全部通过)
- cargo fmt --check: ✓

## 规格符合性

| 验收项 | 结果 | 证据 |
|--------|------|------|
| cargo check --all-targets | ✓ | 0 error |
| cargo clippy --all-targets --all-features -D warnings | ✓ | 0 warning |
| cargo test --all-targets | ✓ | 全绿 |
| cargo fmt --check | ✓ | 一致 |
| helper `///` 已补回 | △ 部分 | provider.rs/tools.rs 逐字对应；fixtures.rs 12 处均补回，但 2 处非 parent 原文（见问题 3） |
| 冗余 re-export 清理（mod.rs:10-21） | ✗ | **未做**：commit 未触及 `tests/common/mod.rs`，re-export 与清理前完全一致 |
| `#![allow(unused_imports)]` 复核（mod.rs:3） | ✗ | **未做**：mod.rs:3 原样保留 |
| 无既有 `#[test]` 删除 / 断言弱化 | ✓ | commit 纯增注释（+19/-0）；`assert!`/`assert_eq!`/`assert_ne!` 总数 577 = parent 577 |
| 零 `src/` 产品代码变更 | ✓ | stat 仅动 `tests/common/{provider,tools,fixtures}.rs` |

## 代码审查

### 问题

1. [Critical] `tests/common/mod.rs:10-21` — **AC「冗余 re-export 清理」未实现**。
   - 影响: 本任务核心目标之一（规格 Goal 第 2 条 / AC 第 6 条）缺失。提交信息 `trim reexports` 与实际 diff 不符（stat 仅 3 文件、无 mod.rs），属交付不完整 + 提交信息误导。
   - 判定依据（已逐个核验 `use common::*` 的 4 个 runtime_loop 文件实际依赖）：
     - **必须保留**（确被 glob 取用，删则编译失败）：
       - `oneshot` → runtime_loop.rs:195
       - `StopReason` → runtime_loop.rs:313,351 / runtime_loop_retry.rs:72,231 / runtime_loop_lifecycle.rs:57,106
       - `AssistantEvent` → runtime_loop.rs:322 / runtime_loop_truncate.rs:180,183
       - `ResourceScope` → runtime_loop.rs:117,155,165-167
       - `Duration` → runtime_loop_retry.rs:83…220 / runtime_loop_lifecycle.rs:77,79
       - `Message` → runtime_loop_lifecycle.rs:52,101（该文件未显式 import Message）
       - `AgentRuntime` / `LoopConfig` / `Model` → runtime_loop_lifecycle.rs:69,72,73
       - `AtomicUsize` / `Ordering` → runtime_loop_truncate.rs:54,74,144,160
       - `AssistantMessage` / `AssistantContent` → runtime_loop_truncate.rs:169,170
     - **可移除**（无任何 glob 消费者在缺显式 import 情况下使用）：`guigu::Agent`（全 tests/ 0 引用）、`AgentHandle`、`ProviderError`、`Tool`、`ToolExecutionMode`、`Arc`（这些均在用到它们的文件里已显式 import）。
   - 建议: 按上述清单逐条清理：保留「必须保留」项，移除「可移除」项（`guigu::core::provider::{AssistantEvent, ProviderError}` → 仅留 `AssistantEvent`；`guigu::core::tool::{ResourceScope, Tool}` → 仅留 `ResourceScope`；`guigu::core::{AgentRuntime, LoopConfig, Model, ToolExecutionMode}` → 去掉 `ToolExecutionMode`；删 `guigu::Agent`、`AgentHandle`、`Arc` 两行）。清理后跑 `cargo check --all-targets` 确认。
   - 备注: 047-review-r1 建议 2 所称「这些类型 re-export 实际冗余」并不准确——大部分确被 glob 取用。故若认为强制清理收益有限，**不得静默跳过**，应回 guigu-planner/PM 复议 AC。

2. [Critical] `tests/common/mod.rs:3` — **AC「`#![allow(unused_imports)]` 复核」未实现**。
   - 影响: allow 仍原样保留，可能继续掩盖未来真实未用导入。
   - 建议: 按 Design Notes 3 移除该 allow 后跑 `cargo clippy --all-targets --all-features -- -D warnings`：0 warning 则永久删除；若触发 unused_imports 则恢复并说明。

3. [Warning] `tests/common/fixtures.rs:67,72` / `tests/common/provider.rs:86` — 补回注释与 parent **非逐字对应**（AC 第 5 条要求「与 parent 版本逐条对应 / 逐条搬回」）。
   - `multi_tool_call_turn`：parent 为两行 `/// 多工具调用 turn：所有 toolCall 的 Start/End 事件 + 末尾**单个** `Done`（message 含全部 toolCall）。真实 provider 一个 turn 只发一个 `Done`。`，当前缩为单行；`multi_tool_call_turn_with_stop`：parent 为五行详述 delta 路径，当前缩为单行。
   - `HangingProvider`：parent（runtime_loop_provider.rs:107-108）为 `/// 挂起 provider：stream() 永不返回（pending() future），用于验证建流阶段的取消/超时（Task 040）…`，当前改为 `/// 永不结束的 provider：用于建流取消测试。`。
   - 影响: 语义信息（delta 累积路径说明、真实 provider 单 Done 约定）丢失，与「逐条搬回」要求不符；非功能缺陷。
   - 建议: 按 parent 原文搬回上述三处（`git show e9ce9ea^:tests/common/runtime_loop_fixtures.rs` / `.../runtime_loop_provider.rs`）。

### 建议（非阻塞）
1. commit message `test(common): restore helper docs and trim reexports` 中 `trim reexports` 与本次 diff 不符，修复轮请让提交信息与实际改动一致。

## closure matrix（本轮冻结，后续轮仅核验此矩阵）

| # | Requirement | Evidence level | Required case |
|---|-------------|----------------|---------------|
| M1 | 补回 helper `///` 注释，位置覆盖 047-review-r1 建议 1 清单 | 静态（diff 对照 parent） | provider/tools/fixtures 对应项均有 `///` |
| M2 | 注释与 parent 逐条对应 | 静态（逐字对照 parent） | fixtures:67,72 与 provider:86 恢复 parent 原文 |
| M3 | 冗余 re-export 清理，保留 glob 实际依赖项 | 编译（cargo check --all-targets） | mod.rs:10-21 按问题 1 清单增删后全绿 |
| M4 | `#![allow(unused_imports)]` 复核 | 静态 + clippy | 移除后 clippy 0 warning（或恢复并说明） |
| M5 | 无 `#[test]` 删除/断言弱化 | 计数 | assert 宏总数 ≥ 577，测试函数数不变 |
| M6 | 零 `src/` 变更 | diff stat | 仅动 `tests/` |

## 结论
- [ ] 通过
- [x] 打回

## 下一步
- Developer 修复：
  1. 实现 `tests/common/mod.rs` re-export 清理（问题 1，按清单保留/移除）。
  2. 复核并处理 `#![allow(unused_imports)]`（问题 2）。
  3. 按 parent 原文恢复 fixtures.rs:67,72 与 provider.rs:86 注释（问题 3）。
  4. （建议）提交信息与实际改动一致。
- 修完跑四门禁后 push，请 reviewer 复审（r2）。
- 门禁本身全绿，本次打回仅因 AC「re-export 清理 / allow 复核」未落地。
