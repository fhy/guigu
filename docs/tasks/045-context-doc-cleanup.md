# Task 045: 上下文预算 API 文档/注释校准

## Background

043 定稿并四绿通过，但 Reviewer 在 r4/r5 报告（`docs/reviews/043-review-r4.md`、`docs/reviews/043-review-r5.md`）中登记了多条非阻塞文档/风格技术债。其中 r4 建议 1（`fits` 文档口径）已随 c496d5c 顺手修复，其余项仍遗留，随 042/043 收尾批次一并清理：

| 来源 | 项 | 问题 |
|------|-----|------|
| 043 r5 建议 1 | `available` 文档 | 仍为「返回可用于消息正文的 token 数」，与新版 `fits`「输入可用预算」措辞不统一 |
| 043 r5 建议 2 | `truncate_to_budget` 公开签名 | 文档出现 `fixed_overhead` 符号，但公开入口无该参数（内部传 0） |
| 043 r4 建议 3 / r5 建议 3 | `context_window` 字段 | 公开字段缺 `///` doc comment |
| 043 r4 建议 4 / r5 建议 3 | `with_overhead` 文档 | 为英文，与仓库其余公开 API 中文文档风格不一 |
| 043 r4 建议 5 | `estimate_tokens("")` | 空串返回 1，空 system/tools 会产生非零 `fixed_overhead`，与 `new(cw)` 略不对称（粗估可接受，需注释说明） |
| 043 r4 建议 6 | `tool_schemas` 拼接 | `context_prep.rs` 用 `format!` 拼接近似序列化，需注释说明近似性 |

## Goal

- 校准 `src/core/context.rs` 与 `src/core/runtime/context_prep.rs` 的公开 API 文档注释，使其与 043 v1.2 定稿口径（`available_input = context_window − reserve_output`；usage 基线路径不叠加 `fixed_overhead`）严格一致。
- 补齐缺失的 `///` doc，统一中文文档风格。
- **纯注释/文档改动，零代码、零测试、零行为变化。**

## Design Notes

### 1. `context.rs` 文档逐点校准

对照 043 v1.2 定稿口径（见 `docs/tasks/043-context-budget-precision.md`）逐点修正：

1. **`available()` 文档**（对应 043 r5 建议 1）：
   - 目标表述：输入可用 token 上限（`context_window − reserve_output_tokens`）。
   - 不得再表述为「可用于消息正文的 token 数」，因返回值为 `window − reserve`，比较对象是含固定开销的**总输入估算**。
2. **`truncate_to_budget` 公开签名文档**（对应 043 r5 建议 2）：
   - 注明「公开入口固定开销为 0（`fixed_overhead = 0`）」，避免公开 API 读者误以为可传固定开销。
   - 内部 `truncate_to_budget_with_overhead(..., self.fixed_overhead)` 保持不变（不改代码）。
3. **`context_window` 字段**（对应 r4 建议 3）：补 `///` doc comment，说明该字段为模型上下文窗口（硬上限口径的基数）。
4. **`with_overhead` 文档**（对应 r4 建议 4）：由英文改为中文，措辞与仓库其余公开 API 一致，并说明 `fixed_overhead` 仅叠加进**无 usage 回退路径**的估算。
5. **`estimate_tokens("")==1` 行为**（对应 r4 建议 5）：在 `estimate_tokens`（或 `fixed_overhead` 计算处）补注释说明「空串估算为 1，故空 system/tools 会产生非零 `fixed_overhead`，属 chars/4 粗估的已知近似，不影响压缩触发正确性」。

### 2. `context_prep.rs` 注释（043 r4 建议 6）

- `src/core/runtime/context_prep.rs` 中 `tool_schemas` 使用 `format!` 拼接近似序列化处，补注释说明其近似性（非严格 JSON 序列化，仅作 token 粗估输入）。

### 3. 零行为变化约束

- 本任务只改 `///` / `//` 注释与文档字符串，**不得修改任何函数体、签名、结构体字段定义、常量值或测试**。
- 不新增/删除 `#[test]`，不移动既有测试。
- 改动不得触发 `cargo fmt --check` 或 `cargo clippy` 新告警（含 `missing_docs` 类告警若启用以 `-D warnings` 门禁判定，补 `///` 后不得引入其它告警）。

## Files

- `src/core/context.rs`（文档/注释校准，仅注释行）
- `src/core/runtime/context_prep.rs`（`tool_schemas` 近似性注释）

## Acceptance Criteria

- [ ] cargo check --all-targets passes
- [ ] cargo clippy --all-targets --all-features -- -D warnings passes（0 warning，补 `///` 后不得引入 `missing_docs` 等新告警）
- [ ] cargo test --all-targets passes（582 passed 基线不减少；`core::context` 21 passed 保持）
- [ ] cargo fmt --check passes
- [ ] `available()` 文档不再含「消息正文」表述，改为「输入可用 token 上限（window − reserve）」
- [ ] `truncate_to_budget` 公开签名文档注明「公开入口固定开销为 0」
- [ ] `context_window` 字段具备 `///` doc；`with_overhead` 文档为中文化
- [ ] `estimate_tokens` 空串行为与 `tool_schemas` 拼接近似性均有注释说明
- [ ] `git diff` 逐行确认仅含注释/文档行改动，无任何逻辑、签名、字段、常量、测试变更
- [ ] 单文件 ≤ 400 行

## 修订记录

- v1.0（2026-09-22，Architect）：依据 043 r4 建议 3/4/5/6 与 r5 建议 1/2/3（`available` 文档、`truncate_to_budget` 公开签名说明、`context_window` 补 `///`、`with_overhead` 中文化、`estimate_tokens` 空串说明、`tool_schemas` 近似性注释）立项。纯文档/注释校准，零行为变化。
