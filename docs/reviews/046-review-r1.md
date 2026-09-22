# Task 046 Review - Round 1

## 基本信息
- 审查时间: 2026-09-22 21:05
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/046-runtime-loop-test-split.md
- 审查提交: `62656a6`（拆分）+ `e82f4bd`（M4 补强）

## 门禁结果
- cargo check --all-targets: ✓
- cargo clippy --all-targets --all-features -- -D warnings: ✓ (0 warning)
- cargo test --all-targets: ✓ (全部通过)
- cargo fmt --check: ✓

## 验收核对

### 1. runtime_loop 场景化拆分（042 r4 建议 3）
- 拆分前 `tests/runtime_loop.rs` = 1364 行；拆分后：
  - `tests/runtime_loop.rs` 350 行（主循环/工具编排：9 用例）
  - `tests/runtime_loop_retry.rs` 230 行（重试/退避/限流：6 用例）
  - `tests/runtime_loop_truncate.rs` 217 行（上下文/长度截断：4 用例）
  - `tests/runtime_loop_lifecycle.rs` 108 行（建流取消/超时：2 用例）
  - `tests/common/runtime_loop_provider.rs` 196 行、`tests/common/runtime_loop_fixtures.rs` 260 行
  - 全部 ≤ 400 行 ✓；单文件用例数上限 9 ≤ 30 ✓
- **零行为变化核对**：
  - 拆分前后 `#[tokio::test]` 数量均为 21，测试函数名一一对应，无删除/合并。
  - `assert!`（16）+ `assert_eq!`（37）宏数量拆分前后完全一致。
  - 逐行集合比对：用例函数体与共享 helper 内容与拆分前逐字一致（仅文件归属/模块 doc 注释变化）。
  - 21 个 runtime-loop 用例实测全部通过（9+6+4+2）。
- "假绿"边界问题（`test_stream_establishment_cancel` 的 `#[tokio::test]` 曾落到切分边界外）已修复并复跑验证，该用例真实执行通过。

### 2. M4 直接断言补强（043 r4 建议 7）
- 新增 `src/core/context/tests.rs:233` `test_fits_true_truncate_preserves_usage_baseline_transcript`，实测通过。
- `core::context` 单测数由 21 → 22（+1），符合 AC。
- **判别性分析**（`with_overhead(100, "x"*40, "", 0, 0)` ⇒ available=100、fixed_overhead=12；transcript=[assistant(usage.input=90), user("follow-up")] ⇒ estimate=93）：
  - 正常路径：`fits==true`（93≤100），`truncate` 命中 `estimate_total≤max` 早返回 ⇒ 列表不变，`assert_eq!` 通过。
  - 若 `truncate` 二次扣减 `fixed_overhead`（max=88）：93>88 → 触发截断，只剩 `[user]`，长度 1≠2 ⇒ 用例失败。**对规格点名的「二次扣减」缺陷有判别性** ✓
- 用例不依赖外部服务/硬编码路径 ✓；为新增用例，未替换或弱化既有 043 用例 ✓。

### 3. 边界约束
- 未修改 `src/` 产品代码；`e82f4bd` 仅改 `#[cfg(test)]` 的 `src/core/context/tests.rs` ✓。

## 代码审查

### 问题
无阻塞问题。

### 建议（非阻塞）
1. `tests/common/runtime_loop_provider.rs:26` / `tests/common/runtime_loop_fixtures.rs:1` — 新 helper 与既有 `tests/common/mod.rs` 存在重复：`FakeProvider`、`SeqTool`、`ConcurrencyTool`、`text_turn`、`tool_call_turn`、`make_config`、`make_runtime`、`user_msg` 在 `common/mod.rs` 已存在一份。规格 Design Notes §1 建议「复用既有 `tests/common/mod.rs`」，当前改为 `include!` 两个新文件，未落回 `common/mod.rs`。
   - 影响：repo 内长期存在两份 FakeProvider/工具定义，易漂移；`include!` 也非 `tests/` 集成测试约定用法（其他测试用 `mod common;`）。
   - 说明：该重复拆分前即存在（旧 `runtime_loop.rs` 内联了自己的 `FakeProvider`），**非本候选引入的回归**，故不阻塞；建议后续把 `runtime_loop_*.rs` 切到 `mod common;` 并把这些 helper 合并进 `common/mod.rs`。
2. `tests/runtime_loop.rs:6` — `test_pure_text_single_turn` 拆分后丢失了原 doc 注释（`/// 纯文本一轮结束：无 toolCall → 单 turn 后退出。`）。纯注释，建议补回以保留可读性。
3. 过程记录 — Done 报告只列了 `62656a6`，但 M4 改动实际在 `e82f4bd`；一个任务跨两个提交，建议在报告中同时列明两个提交号，便于追溯。

## 结论
- [x] 通过
- [ ] 打回

## 下一步
- 无需 Developer 修复。建议 2/3 为可选打磨，可在后续维护任务中一并处理。
