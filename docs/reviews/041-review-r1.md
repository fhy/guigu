# Task 041 Review - Round 1

## 基本信息
- 审查时间: 2026-09-19 18:49
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/041-context-compaction-safety.md
- 审查提交: 3f7642e

## 门禁结果
- cargo check: ✓
- cargo clippy: ✓（`--all-targets --all-features -- -D warnings`，0 warning）
- cargo test: ✓（`--all-targets` 全绿；`compactor` 5 项、`core::context` 17 项通过）
- cargo fmt: ✓

## 代码审查

### 问题

1. **[Warning] src/core/runtime/mod.rs:237-297 — compactor 启用时绕过模型 `context_window` 上限与 `transform_context` 钩子（行为回归）**
   - 现状：compactor 分支直接采用 `plan_context` 的投影作为本轮请求；`plan_context` 在「未超 `policy.budget_tokens`」时原样返回 `transcript.to_vec()`（src/core/context.rs:171-176），既不按 `model.context_window` 截断，也不执行 `transform_context` 钩子。
   - 对照旧实现（HEAD~1）：008 的接入在 `prepare_context` 之后**仍**经 `build_request → build_llm_messages` 应用 `transform_context`（或 `ContextBudget::truncate(context_window)`），即一期契约「每轮请求前按 `context_window` 超限截断（或钩子覆盖）」在 compactor 启用时同样生效。
   - 影响：
     - `CompactionPolicy::default()` 的 `budget_tokens = usize::MAX`（src/core/context.rs:117-124）。当 `compactor: Some(..)` 且策略为默认值时，`estimate_total <= usize::MAX` 恒真 → 请求永不截断，**可超出模型 `context_window`** → provider 端拒绝/报错。这是相对 008 的**回归**。
     - `transform_context` 钩子在 compactor 启用时被静默跳过，与 src/core/runtime/mod.rs:5 的模块契约描述（`或 transform_context 钩子覆盖`）及 008 规格 §4（`prepare_context` 作为增强、再交给钩子投影）不一致。
   - 建议（二选一）：
     - a) compactor 分支产出 `request_messages` 后，仍走一期投影（`transform_context` 钩子，否则 `ContextBudget::new(context_window).truncate`），恢复与旧行为对齐；
     - b) 在 `plan_context` 内将「请求投影」的截断目标钳制为 `min(policy.budget_tokens, model.context_window)`，并明确 `transform_context` 是否继续作用于投影（若是，需在 runtime 侧补调用）。
     - 无论哪种，需明确 `budget_tokens` 与 `context_window` 的配置契约，避免默认 `usize::MAX` 造成无上限。

2. **[Warning] src/core/runtime/mod.rs:217-355 — `run_agent_loop` 139 行，超单函数 80 行上限**
   - 本任务在该函数内新增 `plan_context` 接入 + 提交纪律 match（约 44 行），函数由 94 行增至 139 行。
   - 影响：主循环职责膨胀，`match` 内嵌事件发送与 snapshot 更新，可读性/可测性下降（违反 conventions.md「Single function 80 lines / Extract helper」）。
   - 建议：抽出 `prepare_request_messages(ctx, signal) -> Prepared`（返回 `Aborted` / `Messages(..)` 需 `break` 时以小 enum 或 `Option` 表达），让 `run_agent_loop` 仅编排；或将「提交 + 发 MessageEnd + update_snapshot」抽成 `commit_compaction(ctx, commit)`。

### 建议

1. src/core/compactor.rs:5 — 文档仍引用已重命名的 `context::prepare_context`（现为 `plan_context`），请更新为 `context::plan_context`，避免 stale 引用。
2. src/core/context.rs:255-257 的 `is_tool_call_result_split` 防御分支实际不可达：`boundaries` 仅含 `User` 索引，`messages[boundary]` 必为 `User`，`next_is_tool_result` 恒为 false。当前不产生缺陷（拓扑安全由「切点只落 User」结构性保证），可作为冗余防御保留；若保留建议补注说明其不可达性，或让 `test_truncate_defensive_first_not_user` 真正覆盖该分支语义（现测试仅断言非空与末条保留）。

## 设计疑问（已同步 @bridge-coordinator）

1. **session 持久化语义**：验收标准 #5 表述「transcript 与 session 均被替换为 `[User(summary)] ++ keep`」。当前实现向 session 追加 `MessageEnd(summary)`（append-only 树），磁盘上旧消息节点仍在 → 重载后 session 归约结果与内存压缩后 transcript 不一致，且落盘体积不随压缩下降。Developer 备注已指出这是既有 session 模型边界。请确认该 AC 是否需按 append-only 语义放宽，或需另立任务处理 session 压缩/GC。
2. **压缩期 abort 的可达性**：`plan_context` 等待期间无人 `drain` 命令通道（`AgentCommand::Abort` 经 `drain_commands` 才 `signal.cancel()`），故「用户在压缩期间 abort」在生产路径上要到下一次 `stream_turn` 才被观测；当前 `Err(Cancelled)` 主要由 `shutdown_token` 触发。安全不变式（不丢历史）不受影响，但 spec Background 所述「压缩期间 abort」场景的响应性未被实际接通。请确认是否属本任务范围。

## 结论
- [ ] 通过
- [x] 打回

## 下一步
- Developer 需修复：
  1. compactor 启用路径恢复 `context_window` 上限与 `transform_context` 契约（问题 1）；
  2. `run_agent_loop` 抽出 helper，回到 ≤ 80 行（问题 2）；
  3. 更新 compactor.rs:5 stale 文档引用（建议 1）。
- Coordinator 需澄清：session AC 语义（设计疑问 1）、`budget_tokens` 与 `context_window` 配置契约。
