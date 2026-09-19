# Task 041 Review - Round 2

## 基本信息
- 审查时间: 2026-09-19 19:51
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/041-context-compaction-safety.md (v1.1)
- 审查提交: 3e7db42

## 门禁结果
- cargo check: ✓ (`--all-targets`)
- cargo clippy: ✓ (`--all-targets --all-features -- -D warnings`，0 warning)
- cargo test: ✓ (`--all-targets` 全绿；lib 380 项、compactor 集成 7 项、runtime_loop 16 项、core::context 17 项均通过)
- cargo fmt: ✓

## R1 打回问题复核

### 问题 1（Warning）：compactor 启用时绕过 `context_window` / `transform_context` — 已修复 ✓
- 修复方式：新增 `src/core/runtime/context_prep.rs`，抽出 `apply_final_projection`（单点最终投影：`transform_context` 钩子 → 否则 `ContextBudget::new(context_window).truncate`），compactor 分支与默认分支在 `prepare_request_messages` 末统一经过（context_prep.rs:128-130）。
- 语义对齐：对比 HEAD~1 的 `build_llm_messages_arc`（mod.rs diff），无 compactor 路径 intermediate = `ctx.transcript.clone()`，经 `apply_final_projection` 后与旧行为等价，无双重截断、无语义漂移。
- 覆盖：新增集成测试 `test_compactor_respects_context_window`（预算 `usize::MAX` + 窗口 250，断言最终请求被截断）与 `test_compactor_applies_transform_context_hook`（钩子作为最终权威作用于 compactor 分支产出的 `request_messages`），二者独立运行均 ok，直接覆盖 R1 回归路径。

### 问题 2（Warning）：`run_agent_loop` 139 行超 80 行上限 — 已修复 ✓
- `run_agent_loop` 现 72 行（mod.rs:224-295）≤ 80。
- 上下文准备拆至 `context_prep.rs`；`mod.rs` 295 行 ≤ 400。
- 单文件体量达标；`context_prep.rs` 131 行。

### 建议 1：compactor.rs stale 文档引用 — 已修复 ✓
- compactor.rs:5 已更新为 `context::plan_context`。

### 建议 2：`is_tool_call_result_split` 防御分支注释 + 单测覆盖 — 已修复 ✓
- context.rs:268-277 补注结构性不可达性说明；context.rs:254-255 调用处补注。
- `test_truncate_defensive_first_not_user` 重写为构造畸形 transcript（首条 `Assistant(ToolCall)`），真正断言「不以孤立 ToolResult 开头 + tool call/result 成组保留」，独立运行 ok。

## 代码审查

### 问题
无阻断性问题。

### 建议（非阻断，可选）
1. src/core/runtime/context_prep.rs:110-113 — `plan_context` 返回 `Err(_other)` 的防御分支（注释自述不可达）将 intermediate 设为**完整** `ctx.transcript.clone()`，而非截断投影；虽随后经 `apply_final_projection` 由 `context_window` 兜底截断、且该分支结构性不可达，不影响正确性，但若未来 `plan_context` 引入新的 `Err` 变体，钩子分支会收到未截断的完整 transcript。可考虑改为 `ContextBudget::new(ctx.config.model.context_window).truncate(...)` 以保持「防御分支本身即安全」的一致性。**可选。**

## 结论
- [x] 通过
- [ ] 打回

## 下一步
- Task 041 r1 打回问题全部修复，四道门禁全绿，新增测试真跑断言。
- 交付验收：函数/文件体量、拓扑安全截断、提交纪律、compactor 分支最终投影契约均已满足规格 AC。
