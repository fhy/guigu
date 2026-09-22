# Task 047: tests/common helper 去重 + runtime_loop 切换到 mod common

## Background

046（runtime_loop 测试拆分）已 PASS，但 Reviewer 在 046-review-r1.md 登记一条非阻塞技术债（建议 1）：

- `tests/common/runtime_loop_provider.rs` 与 `tests/common/runtime_loop_fixtures.rs` 与既有 `tests/common/mod.rs` 存在**重复定义**：`FakeProvider`、`SeqTool`、`ConcurrencyTool`、`text_turn`、`tool_call_turn`、`make_config`、`make_runtime`、`user_msg` 在 `common/mod.rs` 已各有一份。
- 046 拆分时 4 个 `runtime_loop*.rs` 用 `include!` 内联这两个新文件，**未落回 `common/mod.rs`**，也偏离 `tests/` 集成测试约定（其余测试统一 `mod common;`）。

同批顺带处理 046-review-r1 建议 2：`tests/runtime_loop.rs` 的 `test_pure_text_single_turn` 拆分后丢失原 doc 注释，补回。

重复根源：旧 `runtime_loop.rs`（1364 行）原本内联了自带 `FakeProvider`，046 拆分时仅机械搬运，未与既有 `common/mod.rs` 合并。长期双份 `FakeProvider` 极易漂移。

## Goal

- 以 `tests/common/mod.rs` 为**唯一** helper 源，把 runtime_loop 特有增强项（`scripted_errors`、`HangingProvider`、特有 fixture/collect helper）合并进去，删除两个重复文件。
- 4 个 `runtime_loop*.rs` 从 `include!` 切到 `mod common;`，恢复 `tests/` 集成测试统一约定。
- 严格**零行为变化**：不删/不改任何 `#[test]` 的断言、参数、setup，测试总数与断言数不减少。

## Design Notes

### 1. 合并方向（唯一源 = `tests/common/mod.rs`）

已核对现有两份定义（`common/mod.rs` 251 行 vs `runtime_loop_provider.rs` 196 行 + `runtime_loop_fixtures.rs` 260 行），差异仅两点：

**A. `FakeProvider` 增强**（`runtime_loop_provider.rs` 版多一字段一构造器）：
- `common/mod.rs` 的 `FakeProvider` 现缺 `scripted_errors: Mutex<VecDeque<ProviderError>>` 字段与 `with_errors(turns, errors)` 构造器（042/043 重试分类测试用）。
- 合并方式：在 `common/mod.rs` 的 `FakeProvider` **新增**该字段 + `with_errors` 构造器；`new()`/`with()` 构造器对 `scripted_errors` 赋空 `VecDeque::new()`。
- 语义不变：空队列 ⇒ `stream()` 不弹 scripted error，既有 `fail_next`/`gate`/`last_context_size` 行为逐字保持。其他集成测试 crate 不设 `scripted_errors`，行为零变化。

**B. runtime_loop 特有 helper**（`common/mod.rs` 完全没有，需整体迁入）：
- `HangingProvider`（040 建流取消/超时用，`stream()` 挂 `pending()`）。
- `tool_call_turn_with_stop`、`multi_tool_call_turn`、`multi_tool_call_turn_with_stop`（040 Length 保护 delta 累积路径用）。
- `tool_result_texts`、`collect_until_agent_end`、`wait_event`（runtime_loop 断言/事件收集用）。

其余重复项（`SeqTool`、`ConcurrencyTool`、`text_turn`、`tool_call_turn`、`make_config`、`make_runtime`、`user_msg`）两份逐字一致，直接保留 `common/mod.rs` 既有定义即可，**删除重复的那份**。

### 2. Size Limits（合并后 `common/mod.rs` 将超 400 行）

合并后 `common/` 总量约 450+ 行，必须拆分为子模块，且**不得破坏其它测试 crate 的既有 pub 路径**（它们用 `common::FakeProvider`、`common::text_turn` 等）。

拆分约定：
- `tests/common/mod.rs` 保留为入口：`mod provider; pub use provider::*;` 等 re-export 方式，保证 `common::FakeProvider`、`common::HangingProvider`、`common::text_turn`、`common::tool_call_turn_with_stop` 等既有名称对外可见。
- 建议子模块划分（以实际职责为准，Developer 可微调，但每个文件 ≤ 400 行、单文件 ≤ 30 `#[test]`）：
  - `tests/common/provider.rs`：`FakeProvider`（含增强）+ `HangingProvider`。
  - `tests/common/tools.rs`：`SeqTool` + `ConcurrencyTool`。
  - `tests/common/fixtures.rs`：`text_turn` / `tool_call_turn` / `tool_call_turn_with_stop` / `multi_tool_call_turn` / `multi_tool_call_turn_with_stop` / `make_config` / `make_runtime` / `user_msg` / `line` / `tool_result_texts` / `collect_until_agent_end` / `wait_event`。
- `common/mod.rs` 顶部 `#![allow(dead_code)]` 保留（不同测试 crate 用不同子集）。

### 3. runtime_loop 文件切换

4 个文件（`runtime_loop.rs` / `_retry.rs` / `_truncate.rs` / `_lifecycle.rs`）：
- 删除顶部 `#![allow(dead_code)]` 与两条 `include!("common/runtime_loop_*.rs")`。
- 改为 `mod common;`，用 `use common::{...}` 引入所需符号（机械改引用名，不改用例体）。
- 原由被 include 文件顶部 import 引入的类型（如 `AgentHandle`、`Agent`、`Arc`、`AtomicUsize`、`Message` 等），切换后需在各测试文件顶部自行补 `use`；不得因此改动任何用例逻辑。

### 4. 零行为变化硬约束

- 仅重组 helper 归属与引用路径，不删除/合并/改写任何 `#[test]`。
- `assert!` + `assert_eq!` 宏总数不减少；`cargo test --all-targets` 通过总数与拆分后基线一致（`runtime_loop` 21 项、`core::context` 22 项等均保持）。
- 不动 `src/` 产品代码（本任务只动 `tests/`）。

## Files

- `tests/common/mod.rs`（拆分入口 + re-export，可能新增 `provider.rs`/`tools.rs`/`fixtures.rs` 子模块）
- `tests/common/runtime_loop_provider.rs`（**删除**，内容并入 `common/`）
- `tests/common/runtime_loop_fixtures.rs`（**删除**，内容并入 `common/`）
- `tests/runtime_loop.rs` / `runtime_loop_retry.rs` / `runtime_loop_truncate.rs` / `runtime_loop_lifecycle.rs`（`include!` → `mod common;` + 补 `use`）

## Acceptance Criteria

- [ ] cargo check --all-targets passes
- [ ] cargo clippy --all-targets --all-features -- -D warnings passes（0 warning）
- [ ] cargo test --all-targets passes（`runtime_loop` 21 项 + `core::context` 22 项通过总数不变；其它集成测试 crate 全部仍编译+通过）
- [ ] cargo fmt --check passes
- [ ] `tests/runtime_loop*.rs` 不再出现 `include!`；`tests/common/runtime_loop_provider.rs` / `runtime_loop_fixtures.rs` 已删除
- [ ] `common/` 下每个文件 ≤ 400 行；单测试文件 ≤ 30 `#[test]`
- [ ] 其它测试 crate 对 `common::FakeProvider` / `common::text_turn` 等既有 pub 路径的引用零破坏（仍编译通过）
- [ ] 无既有 `#[test]` 被删除或断言弱化；`assert!`/`assert_eq!` 宏总数不减少
- [ ] `test_pure_text_single_turn` 原 doc 注释（`/// 纯文本一轮结束：无 toolCall → 单 turn 后退出。`）已补回

## 修订记录

- v1.0（2026-09-22，Architect）：依据 046-review-r1 建议 1（`tests/common/` helper 重复 + `include!` 偏离约定）+ 建议 2（补回 doc 注释）立项。纯测试 helper 重组 + 引用方式切换，零产品行为变化。
