# Task 043 Review - Round 1

## 基本信息
- 审查时间: 2026-09-22 19:27
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/043-context-budget-precision.md
- 候选提交: b3dc1ea `feat: 精确化上下文预算计算`

## 门禁结果
- cargo check --all-targets: ✓
- cargo clippy --all-targets --all-features -- -D warnings: ✓（0 warning）
- cargo test --all-targets: ✓（全绿；`core::context` 19 passed）
- cargo fmt --check: ✓

## 规格符合性概览
- 预算公式 `available = context_window - reserve_output - (system+tools+protocol)` 已实现（`ContextBudget::with_overhead`/`available`，context.rs:106-144）。✓
- usage 基线优先：`estimate_total` 取最后一条 `AssistantMessage.usage.input` 作基线 + 新增消息增量估算，无 usage 回退全量 `chars/4`（context.rs:65-87）。✓
- runtime 将 system/tool/协议/输出预留纳入最终投影（context_prep.rs:40-59）。✓
- `CompactionPolicy` 两新增字段 + `Default`；所有构造点已补齐（src 内 15 处 + tests 均更新）。✓
- 产品代码无新增 `unwrap()`。✓
- 体量：context.rs 369 行（≤400）；tests.rs 13 个 `#[test]`（≤30）；函数均 ≤80 行。✓

## 代码审查

### 问题（须修复）
1. [Warning] src/core/context.rs:106,140,93-94,161-162 — **新增公开 API 缺 `///` 文档注释**。
   - 具体：`ContextBudget::with_overhead`（106）、`ContextBudget::available`（140）、新增字段 `ContextBudget.fixed_overhead`/`reserve_output_tokens`（93-94）、`CompactionPolicy.reserve_output_tokens`/`protocol_wrapper_tokens`（161-162）均无文档。
   - 影响: 违反 `docs/conventions.md`「Public APIs require `///` doc comments」；这些是新增公开面，使用者无从了解字段单位/语义（尤其 `protocol_wrapper_tokens` 的用途与量级）。
   - 建议: 为上述公开项补 `///`：说明 `fixed_overhead` 的构成（system+tools+protocol 固定开销）、`reserve_output_tokens` 为输出预留、`protocol_wrapper_tokens` 覆盖请求体骨架/role-type 包装的固定开销。

2. [Warning] src/core/context.rs:124-131 — **`ContextBudget::estimate`/`fits` 与 `truncate` 的估算口径不一致（本候选引入）**。
   - 具体：`truncate`（137）委托 `truncate_to_budget`→`estimate_total`，后者**优先** usage 基线；而 `estimate`（124）/`fits`（129）仍是纯 `estimate_message_tokens` 求和，**未**接入基线。于是当 transcript 携带 `usage.input` 时，可能出现 `budget.fits(msgs) == true` 但 `budget.truncate(msgs)` 仍会截断的自相矛盾。
   - 影响: `ContextBudget`/`fits` 是 `lib.rs`/`core/mod.rs` 公开导出，语义分裂对调用方是隐患（本候选之前二者口径一致，属回归）。
   - 建议: 让 `estimate` 复用基线逻辑（内部改为调用 `estimate_total` 的同一套口径），或明确文档化「`estimate` 为不含 usage 的纯正文粗估」。二选一即可。

3. [Warning] src/core/context.rs:153 — **`budget_tokens` 文档注释已过时**。
   - 具体: 注释仍写「触发压缩的 token 阈值（粗估）」，但规格 §3 明确本任务后语义为「消息正文可用 token（总窗口 = budget_tokens + reserve_output + fixed_overhead）」。
   - 建议: 更新该字段文档，说明新语义与 `reserve_output_tokens`/`fixed_overhead` 的加和关系，避免误配。

### 建议（非阻塞）
1. （**需 Coordinator 确认的设计疑问**，见下方「设计疑问」）usage 基线与 fixed_overhead 可能重复扣减。
2. 测试覆盖与规格 AC 的偏差：
   - AC5「超 available 触发压缩」：`test_budget_includes_reserve_and_fixed_overhead` 只断言 `fits()==false`，**未**覆盖 `with_overhead` 路径下的 `truncate` 实际触发。
   - AC6「无 usage → 全量 `chars/4` 回退」：仅由既有 plan_context 测试间接覆盖，无直接断言。
   - AC7「空/短/长文本边界」：仅一个用例（system/tools 均 "1234"），未覆盖空串与长串。
3. src/core/context.rs:113-115 — `estimate_tokens` 对空串返回 1，故 `with_overhead(cw,"","",0,0)` 得 `available=cw-2`，与 `new(cw)` 的 `cw` 不一致；空输入是否应计 0 可斟酌。

## 设计疑问（提请 Coordinator）
- 供应商 `usage.input` 即 OpenAI `prompt_tokens` / Anthropic `input_tokens`（src/adapters/openai/events.rs:148、anthropic/events.rs:86-90），**已包含 system prompt 与工具 schema**。
- 而硬上限路径把二者又各扣一次：`estimate = 基线(含 system+tools) + 新增消息` 对比 `available = window - reserve - (system+tools+protocol)`，等于对 system+tools **重复扣减**，会比规格本意更早截断/压缩。
- 请确认这是规格刻意的「保守」取舍（那当前实现符合规格，不作阻塞项），还是应改为「基线相对比较」（基线仅代表历史正文，或 available 不再扣 fixed_overhead）。如需调整，属设计/范围问题，请更新规格后再由 Developer 修改。

## 结论
- [ ] 通过
- [x] 打回

## 下一步
- Developer 修复：问题 1（补公开 API 文档）、问题 2（统一 `estimate`/`fits` 与 `truncate` 口径）、问题 3（更新 `budget_tokens` 文档）。
- 建议项 2、3 可一并补齐（尤其 AC5/AC6/AC7 的边界断言）；项 1（重复扣减）待 Coordinator 定性。
