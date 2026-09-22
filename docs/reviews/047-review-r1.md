# Task 047 Review - Round 1

## 基本信息
- 审查时间: 2026-09-22 21:40
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/047-tests-common-dedup.md
- 审查对象: commit `e9ce9ea` refactor: deduplicate runtime loop test helpers

## 门禁结果
- cargo check --all-targets: ✓
- cargo clippy --all-targets --all-features -- -D warnings: ✓ (0 warning)
- cargo test --all-targets: ✓ (全绿；runtime_loop 4 crate 9+6+4+2=21 项通过)
- cargo fmt --check: ✓

## 规格符合性

| 验收项 | 结果 | 证据 |
|--------|------|------|
| `common/` 为唯一 helper 源，合并增强项 | ✓ | `provider.rs` 含 `scripted_errors` + `with_errors` + `HangingProvider`；`fixtures.rs` 含全部 runtime_loop 特有 fixture |
| 删除两个重复文件 | ✓ | `tests/common/runtime_loop_provider.rs` / `runtime_loop_fixtures.rs` 已删除 |
| 4 个 runtime_loop 切到 `mod common;` | ✓ | 四个文件均已 `mod common; use common::*;`，无 `include!` |
| 无 `include!` 残留 | ✓ | `grep -rn "include!" tests/` 无结果 |
| 零行为变化（断言数不减少） | ✓ | `assert!`/`assert_eq!`/`assert_ne!` 宏总数 577 = parent 577；测试函数数 9/6/4/2 = parent |
| `common/` 每文件 ≤400 行 | ✓ | mod.rs 21 / provider.rs 98 / tools.rs 66 / fixtures.rs 219 |
| 单测试文件 ≤30 `#[test]` | ✓ | 全 tests/ 无超标 |
| 其它 crate 既有 pub 路径零破坏 | ✓ | `echo_agent/server/remote/session/acp_sse` 等经 `common::FakeProvider`/`text_turn`/`make_runtime` 全部编译通过 |
| `test_pure_text_single_turn` doc 注释补回 | ✓ | `/// 纯文本一轮结束：无 toolCall → 单 turn 后退出。` 已恢复 |
| 无 `src/` 产品代码改动 | ✓ | stat 仅动 `tests/` |

### 合并正确性核对（逐项对照 parent 定义）
- `FakeProvider`：新增 `scripted_errors: Mutex<VecDeque<ProviderError>>` 字段 + `with_errors` 构造器；`new`/`with` 对空队列初始化；`stream()` 中 gate → fail_next → scripted_errors 顺序与旧 `runtime_loop_provider.rs` 逐字一致。空队列对其它 crate 语义零变化。✓
- `HangingProvider`：`stream()` 挂 `pending()` 保留。✓
- `text_turn` / `tool_call_turn`：`tool_call_turn` 委托 `tool_call_turn_with_stop(.., Completed)`，事件序列与旧版一致。✓
- `multi_tool_call_turn` 委托 `multi_tool_call_turn_with_stop(calls, &[], Completed)`，delta 路径关闭，等价旧版无 delta 行为。✓
- `multi_tool_call_turn_with_stop` delta 累积路径（Start 空参→Delta→End）与旧 `runtime_loop_fixtures.rs` 一致。✓
- `make_config` / `make_runtime` / `user_msg` / `line` / `tool_result_texts` / `collect_until_agent_end` / `wait_event` 语义逐字保留。✓

## 代码审查

### 问题
无阻塞性（Critical/Warning）缺陷。合并语义正确，零行为变化经断言数与测试函数数双重量化验证。

### 建议（非阻塞）
1. `tests/common/provider.rs:11` / `tests/common/tools.rs:8,35` / `tests/common/fixtures.rs:12,31,35,64,68,113,121,142,151,160,173,200` — 合并时丢弃了旧 `common/mod.rs` 与 `runtime_loop_fixtures.rs` 上的 `///` 文档注释（如 `/// 脚本化 provider：...`、`/// 纯文本 turn 脚本：[TextDelta, Done]。`）。
   - 影响: 本次为纯重组，旧文件对 helper 均有文档注释，合并后大量丢失；与 conventions.md「Public APIs require `///` doc comments」的既有风格不一致，且 046 恰恰因丢注释立了本任务。属文档质量回归，非功能缺陷。
   - 建议: 从 parent 版本补回这些 `///` 文档注释（可放子模块对应项上）。
2. `tests/common/mod.rs:10-21` — 除规格要求的 helper re-export 外，额外 `pub use` 了 `guigu::Agent`、`AgentHandle`、`Message`、`Arc`、`AtomicUsize`、`Ordering`、`Duration`、`oneshot` 等 std/guigu 类型。
   - 影响: 规格设计说明（Design Notes 2、3）的原意是 helper re-export + 各测试文件自行补 `use`；且四个 runtime_loop 文件确已各自补了 `use std::sync::Arc;` 等，故这些类型 re-export 实际冗余，额外扩大了 `common` 对外耦合面。
   - 建议: 移除多余的 std/guigu 类型 re-export，仅保留 helper 与测试确实依赖的少量类型（若确有 crate 仅靠 `use common::*` 取用这些类型，则保留即可）。可顺带复核 `#![allow(unused_imports)]`（mod.rs:3）是否仍必要——`pub use` 本身不触发 unused_imports，该 allow 可能已无必要并会掩盖未来真实未用导入。

## 结论
- [x] 通过
- [ ] 打回

## 下一步
- 全部验收项通过，任务可标记完成。
- 上述 2 条为非阻塞建议，可由 PM/Planner 决定是否并入本任务或另立测试 doc/清理小项；不影响零行为变化目标达成。
