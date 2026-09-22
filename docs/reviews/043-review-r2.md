# Task 043 Review - Round 2

## 基本信息
- 审查时间: 2026-09-22 19:40
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/043-context-budget-precision.md（v1.1）
- 候选提交: 88014af `docs(task-043): 依据 Review R1 设计疑问定稿预算口径`
  （含 R1 代码/测试修复 + 规格 v1.1 变更）

## 门禁结果
- cargo check --all-targets: ✓
- cargo clippy --all-targets --all-features -- -D warnings: ✓（0 warning）
- cargo test --all-targets: ✓（全绿；`core::context` 全通过）
- cargo fmt --check: ✓

## R1 阻塞项复核
1. 新增公开 API 文档 —— **已修复** ✓
   - `ContextBudget.fixed_overhead`/`reserve_output_tokens`（context.rs:93-96）、`with_overhead`（108-112）、`estimate`（130）、`fits`（135）、`available`（147）、`CompactionPolicy.budget_tokens`/`reserve_output_tokens`/`protocol_wrapper_tokens`（161-172）均已补 `///`。
2. `estimate`/`fits` 与 `truncate` 口径统一 —— **已修复** ✓
   - `estimate`（131-133）改为复用 `estimate_total`；`fits`（136-138）经 `estimate`；`truncate`（143-145）经 `truncate_to_budget`→`estimate_total`。三者同口径。
3. `budget_tokens` 文档更新 —— **已修复** ✓（context.rs:161）

## 代码审查

### 问题（须修复）

1. [Critical] src/core/context.rs:148-152（`available`）配合 65-87（`estimate_total`）——**usage 基线路径对 `fixed_overhead` 仍二次扣减，与已批准规格 v1.1 §1/§2（及 AC5/AC6）冲突**。
   - 现状：`estimate_total` 的 usage 分支返回 `baseline + 新增增量`（spec 已述：`baseline` = provider `input_tokens`，**已含** system+tools+protocol）；而 `available()` 恒定 `context_window − reserve_output_tokens − fixed_overhead`。故 usage 路径实际比较为
     `baseline + 增量  ≤  window − reserve − fixed_overhead`，
     比规格本意多扣一次 `fixed_overhead`。
   - 规格对照：`docs/tasks/043-context-budget-precision.md:19` 定义 `available_input = context_window − reserve_output_tokens`；`:32`、`:41`、修订记录 v1.1（:87）明确「`fixed_overhead` 仅在无 usage 回退路径叠加，**usage 基线路径不再二次扣减**」。
   - 影响：usage 路径会**早于规格本意触发压缩/截断**（保守方向），与本任务「预算精确化」目标相悖；且 `budget_tokens`/`available` 公开 API 语义与规格不符。这正是 R1 提请 Coordinator 定性的设计疑问，现已由 v1.1 定稿 → 代码应随规格修正。
   - **备注**：回退路径在两种写法下比较结果等价（`body + fixed_overhead + reserve ≤ window` ⇔ `body ≤ window − reserve − fixed_overhead`），差异**仅**出现在 usage 路径。因此修复需同时满足：`available()` 回到 `window − reserve`，并把 `fixed_overhead` 叠加进 `estimate_total` 的**回退分支**；仅改一侧会破坏回退路径。
   - 建议：按 v1.1 §1 实现「总输入单一口径」：
     `available = context_window − reserve_output_tokens`；
     回退分支 `estimate_total = fixed_overhead + 全量正文估算`；
     usage 分支保持 `baseline + 增量`（不再叠加 `fixed_overhead`）。
     同步修正 `src/core/context/tests.rs:166-170`（该用例断言 `available()==66`，即旧口径，需随修复更新）。
   - 若 Coordinator 实际意在保留当前「保守（二次扣减）」行为，则该行为与现规格文本矛盾，请先**修订规格**再定收敛方向；否则请 Developer 按规格修正代码。

### 建议（非阻塞）
1. src/core/context.rs:92 — 既有公开字段 `context_window` 仍无 `///` 文档（R1 未列，属既有面；建议顺手补齐）。
2. src/core/context.rs:108-112 — `with_overhead` 文档为英文，仓库其余公开 API 文档为中文，建议统一语言。
3. src/core/context.rs:25-27 — `estimate_tokens("")` 返回 1，故 `with_overhead(cw,"","",0,0)` 得 `available=cw−2`，与 `new(cw)` 不一致（R1 建议 3，沿用，非阻塞）。
4. src/core/runtime/context_prep.rs:40-51 — `tool_schemas` 用 `format!("{}{}{:?}", name, description, parameters)` 拼接近似序列化，与 provider 实际请求体骨架有出入；作为粗估可接受，建议注释说明其为近似。

## 测试证据 closure matrix（冻结）

本轮首次因「spec 契约」而非仅覆盖率打回，冻结如下矩阵；后续轮次仅核验本矩阵（除新提交引入回归外不新增阻塞项）。

| # | Requirement（出处） | Evidence level | Required case | 阻塞 |
|---|--------------------|----------------|---------------|------|
| M1 | usage 基线路径不再二次扣减 `fixed_overhead`（spec v1.1 §1/§2、修订记录） | 实现检视 + 单测 | 含 `usage.input` 且 `fixed_overhead>0` 的 transcript：`fits`/`truncate` 触发点等价于「总输入 ≤ `window − reserve`」 | 是 |
| M2 | `available_input = context_window − reserve_output`（spec v1.1 §1；AC5） | 单测 | `ContextBudget::with_overhead(...)` 断言 `available() == window − reserve` | 是 |
| M3 | 回退路径 `estimate_total = fixed_overhead + 全量 chars/4`（AC6） | 单测 | 无 usage transcript，断言 `fixed_overhead` 计入触发判断 | 是 |
| M4 | `estimate`/`fits`/`truncate` 口径一致（AC 第 3 条） | 单测 | 携带 `usage.input` 时 `fits==true ⇒ truncate` 不截断 | 否（R1 已列建议项） |

## 体量与规范
- context.rs 380 行（≤400）✓；`context/tests.rs` 13 个 `#[test]`（≤30）✓；函数均 ≤80 行 ✓。
- 产品代码无**新增** `unwrap()` ✓（`truncate_to_budget` 中既有 `.expect("boundaries is non-empty")` 为 R1 前既有代码，非本次引入）。
- `CompactionPolicy` 构造点：src/tests 内 21 处，均已补齐两新字段；`ContextBudget::with_overhead` 仅 runtime 生产点 1 处 ✓。

## 结论
- [ ] 通过
- [x] 打回

## 下一步
- Developer 修复：问题 1（按规格 v1.1 修正 usage 路径口径 → `available = window − reserve`、回退分支叠加 `fixed_overhead`，并补 M1/M2/M3 回归断言，更新 tests.rs:166-170 旧口径断言）。
- 若认为应保留「保守（二次扣减）」行为，属规格文本与实现冲突，请 @guigu-planner 先修订规格再定收敛方向。
- 建议项 1-4 可一并处理（非阻塞）。
