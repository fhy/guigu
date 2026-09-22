# Task 043 Review - Round 4

## 基本信息
- 审查时间: 2026-09-22 20:20
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/043-context-budget-precision.md（v1.2）
- 候选提交: a3c65c5 `fix: align context budget overhead semantics`
- 触发: Coordinator 裁决（维持规格 v1.2，按规格修正代码）后显式请求「按冻结 closure matrix（M1–M4）+ v1.2 复审 a3c65c5」。
- 代码状态说明: HEAD = 713cfde；a3c65c5 之后仅 3494f03 / 713cfde 两个 **docs-only** 提交，无 src 变更。故本次复审的代码与 R3 完全一致，本轮为 Coordinator 要求的 **M1–M4 完整闭环确认**（R3 表格仅显式列 M1–M3）。

## 门禁结果
- cargo check --all-targets: ✓
- cargo clippy --all-targets --all-features -- -D warnings: ✓（0 warning）
- cargo test --all-targets: ✓（**582 passed / 0 failed**；`core::context` 21 passed）
- cargo fmt --check: ✓

## 冻结 closure matrix（M1–M4）复核

| # | Requirement | 结论 | 证据 |
|---|-------------|------|------|
| M1 | usage 基线路径不再二次扣减 `fixed_overhead` | **闭环** ✓ | context.rs:74-81（usage 分支仅 `input + Σ后续消息`，不叠加）；context.rs:151-154（`available = window − reserve`）；单测 `test_usage_baseline_does_not_double_deduct_overhead`（tests.rs:208） |
| M2 | `available_input = context_window − reserve_output` | **闭环** ✓ | context.rs:151-154；单测 `test_budget_includes_reserve_and_fixed_overhead`（tests.rs:166）断言 `available()==90`（window 100 − reserve 10） |
| M3 | 回退路径 `estimate_total = fixed_overhead + 全量 chars/4` | **闭环** ✓ | context.rs:82-88（fallback 分支 `fixed_overhead + Σ正文`）；单测 `test_fallback_estimate_includes_fixed_overhead`（tests.rs:232） |
| M4 | `estimate`/`fits`/`truncate` 口径一致 | **闭环（构造性）** ✓ | 见下 | 

### M1 区分度核验
`test_usage_baseline_does_not_double_deduct_overhead`：`with_overhead(100, "x"*40, "", 0, 0)` → `fixed_overhead=12`、`available=100`；transcript `[assistant(usage.input=90)]` → `estimate=90`。
- 正确口径：`90 ≤ 100` → `fits==true`。旧（二次扣减）口径：`available=88` → `fits==false`，断言会失败。**有真实区分度**。✓

### M2 断言有效性
`test_budget_includes_reserve_and_fixed_overhead`：`with_overhead(100, "1234", "1234", 10, 20)` → `available=100−10=90`（旧值 66 已更新）。该断言直接锁 `available = window − reserve`。✓

### M3 断言有效性
`test_fallback_estimate_includes_fixed_overhead`：无 usage transcript `[user("x"*400)]` → `estimate_total = 12 + 101 = 113 > available(100)` → `fits==false`；若 `fixed_overhead` 未计入则 `101 ≤ 100` 仍 false，故另加 `estimate(&[]) >= fixed_overhead` 直接锁「回退含固定开销」。✓

### M4 口径一致性（构造性论证）
- `estimate`（context.rs:134-136）= `estimate_total(msgs, self.fixed_overhead)`；
- `fits`（139-141）= `estimate(msgs) <= available()`；
- `truncate`（146-148）= `truncate_to_budget_with_overhead(msgs, available(), self.fixed_overhead)`，其内部首判（context.rs:303）为 `estimate_total(msgs, self.fixed_overhead) <= available()`，与 `fits` **同一表达式**；为真即原样返回、不截断。
- ⇒ 「`fits==true` ⇒ `truncate` 不截断」对含 `usage.input` 与非 usage 两类 transcript **均恒成立**，fits/truncate 无口径分裂。✓

## 规格符合性（v1.2）
- §1 总输入单一口径：`estimate_total(messages, fixed_overhead)` 以参数传入（context.rs:65）；`truncate` 经 `truncate_to_budget_with_overhead(..., self.fixed_overhead)` 同口径（147）；公开 `truncate_to_budget` 传 `0`（295，签名/语义兼容未变）。✓
- §2 usage 优先 + 无 usage 回退；基线路径不叠加固定开销。✓
- §3 `plan_context`（233）比较 `estimate_total(transcript, 0)`（软阈值不叠加 system/tools）；`CompactionPolicy` 两新增字段 additive、`Default` 保守（reserve=1024/protocol=128）。✓
- §4 仅改触发预算，未改提交/截断机制。✓
- runtime 生产接线仅 `context_prep.rs:52`（`with_overhead`）一处，硬上限口径由最终投影承担。✓

## 体量与规范
- context.rs 389 行（≤400）✓；`context/tests.rs` 447 行（无 30 test 上限约束，13 个 `#[test]`）+ `tests_truncate.rs`（5 个）✓；函数均 ≤80 行 ✓。
- 产品代码无 `unwrap()`（context.rs `grep` 零命中）✓；仅存既有 `.expect("boundaries is non-empty")`（context.rs:334，非本候选引入）。
- `Usage.input: u64`（message.rs:77），与 `estimate_total` 的 u64 累加一致，无溢出/widen 隐患 ✓。

## 建议（非阻塞，沿用 R2/R3，Coordinator 已同意顺带处理）
1. src/core/context.rs:138 — `fits` 文档「在扣除固定开销和输出预留后的预算内」与 v1.2 口径不符：`available()` 只扣 `reserve`，`fixed_overhead` 是叠加进**回退估算**而非从 `available` 扣。建议改为「总输入估算（含固定开销）≤ `window − reserve`」。
2. src/core/context.rs:150 — `available` 文档「返回可用于消息正文的 token 数」欠精确（返回值为 `window − reserve`，与含固定开销的总输入估算比较）。建议改为「消息正文可用 token 上限（`window − reserve`）」。
3. src/core/context.rs:95 — 既有公开字段 `context_window` 仍无 `///`（既有面，非本候选引入）。
4. src/core/context.rs:111-115 — `with_overhead` 文档为英文，与仓库其余公开 API 中文风格不一。
5. src/core/context.rs:24-27 — `estimate_tokens("")==1`，故空 system/tools 会产生非零 `fixed_overhead`，与 `new(cw)` 略不对称（粗估可接受）。
6. src/core/runtime/context_prep.rs:40-51 — `tool_schemas` 为 `format!` 拼接近似序列化，建议注释说明其近似性。
7. （可选，M4 补强）可增一条「含 `usage.input` 且 `fits==true` ⇒ `truncate` 返回原列表」的直接断言；M4 在 R2 矩阵中即为**非阻塞**，本轮不据此打回。

## 结论
- [x] 通过
- [ ] 打回

## 下一步
- 无阻塞项。Task 043 确认通过，可转 [x]。
- 建议项 1-7 均为非阻塞清理，可并入后续维护（建议项 1/2 由 Coordinator 提议 @guigu-worker 顺手校准）。
