# Task 043 Review - Round 3

## 基本信息
- 审查时间: 2026-09-22 20:05
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/043-context-budget-precision.md（v1.2）
- 候选提交: a3c65c5 `fix: align context budget overhead semantics`

## 门禁结果
- cargo check --all-targets: ✓
- cargo clippy --all-targets --all-features -- -D warnings: ✓（0 warning）
- cargo test --all-targets: ✓（386 passed / 0 failed；`core::context` 21 passed）
- cargo fmt --check: ✓

## R2 阻塞项复核（冻结矩阵 M1/M2/M3）

| # | Requirement | 结论 | 证据 |
|---|-------------|------|------|
| M1 | usage 基线路径不再二次扣减 `fixed_overhead` | **已闭环** ✓ | context.rs:65-90（usage 分支仅 `baseline + 增量`，不叠加）；context.rs:151-154（`available` 撤销 `- fixed_overhead`）；单测 `test_usage_baseline_does_not_double_deduct_overhead`（tests.rs） |
| M2 | `available_input = context_window − reserve_output` | **已闭环** ✓ | context.rs:151-154；单测 `test_budget_includes_reserve_and_fixed_overhead` 断言 `available()==90`（window 100 − reserve 10，旧值 66 已更新） |
| M3 | 回退路径 `estimate_total = fixed_overhead + 全量 chars/4` | **已闭环** ✓ | context.rs:82-88（fallback 分支 `u64::from(fixed_overhead) + Σ正文`）；单测 `test_fallback_estimate_includes_fixed_overhead` |

### M1 断言有效性核验
`test_usage_baseline_does_not_double_deduct_overhead`：`with_overhead(100, "x"*40, "", 0, 0)` → `fixed_overhead=12`、`available=100`；transcript `[assistant(usage.input=90)]` → `estimate=90`。
- 正确口径：`90 ≤ 100` → `fits==true`，断言通过。
- 旧（二次扣减）口径：`available=88` → `fits==false`，断言**会失败**。
- 该用例对 R2 缺陷具有真实区分度，非平凡通过。✓

## 代码审查

### 口径一致性（AC 第 3 条）
- `estimate`（context.rs:134-136）→ `estimate_total(msgs, self.fixed_overhead)`
- `fits`（139-141）→ 经 `estimate`，`<= available()`
- `truncate`（146-148）→ `truncate_to_budget_with_overhead(msgs, available(), self.fixed_overhead)`
- `plan_context`（233）→ `estimate_total(transcript, 0)`（`budget_tokens` 正交软阈值，按规格 v1.2 §3 不叠加固定开销）
- 公开 `truncate_to_budget`（294-296）→ `..._with_overhead(..., 0)`，签名与语义兼容未变。

三者已共用同一 `estimate_total` 口径，fallback 路径不再分裂。✓

### 规格符合性
- §1 预算公式（总输入单一口径）✓
- §2 usage 优先 + 无 usage 回退 ✓
- §3 `CompactionPolicy` 两新增字段 additive，`Default` 保守（reserve=1024/protocol=128）✓；`budget_tokens` 文档已更新为「消息正文可用 token；总窗口还包括输出预留和固定请求开销」✓
- §4 边界：仅改触发预算，未改提交/截断机制 ✓

### 体量与规范
- context.rs 389 行（≤400）✓；`context/tests.rs` 9 + `tests_truncate.rs` 5 = 14 个 `#[test]`（≤30）✓；函数均 ≤80 行 ✓
- 产品代码无**新增** `unwrap()` ✓。仅存 context.rs:334 `.expect("boundaries is non-empty")`，为 R1 前既有代码，非本次引入。
- `with_overhead` 生产调用点仅 `src/core/runtime/context_prep.rs:52` 一处 ✓。

### 建议（非阻塞，沿用 R2，未强制）
1. src/core/context.rs:95 — 既有公开字段 `context_window` 仍缺 `///` 文档（既有面，非本候选引入）。
2. src/core/context.rs:111-115 — `with_overhead` 文档为英文，与仓库其余公开 API 中文文档风格不一。
3. src/core/context.rs:24-27 — `estimate_tokens("")==1`，故 `with_overhead(cw,"","",0,0)` 得 `available=cw−1`、`fixed_overhead=2`（空串各计 1），与 `new(cw)` 不一致；粗估可接受。
4. src/core/runtime/context_prep.rs:52 附近 — `tool_schemas` 为格式化拼接近似，建议注释说明其近似性。

## 结论
- [x] 通过
- [ ] 打回

## 下一步
- 无阻塞项。Task 043 可转 [x]。建议项 1-4 属可选清理，可并入后续维护。
