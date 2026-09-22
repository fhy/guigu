# Task 042 Review - Round 3

## 基本信息
- 审查时间: 2026-09-22 19:07
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/042-provider-error-retry-class.md
- 候选提交: c4f8bbf `test(runtime): cover provider retry classifications`
- 上轮报告: docs/reviews/042-review-r2.md（round 2 打回）

## 门禁结果
- cargo check: ✓ (`cargo check --all-targets`)
- cargo clippy: ✓ (`cargo clippy --all-targets --all-features -- -D warnings`，0 warning)
- cargo test: ✓ (`cargo test --all-targets`，0 failed；runtime_loop 目标 19 passed)
- cargo fmt: ✓ (`cargo fmt --check`)

本轮新增 3 个 runtime 用例均真实执行通过：
- `test_permanent_provider_error_is_not_retried` ✓
- `test_rate_limited_retry_after_is_capped` ✓
- `test_retry_backoff_can_be_cancelled` ✓

## 代码审查（对照冻结 Closure Matrix）

| # | Requirement | 上轮 | 本轮 Evidence | 判定 |
|---|-------------|------|---------------|------|
| T1 | `retry_class` 映射表逐条单测 | covered | 维持（provider.rs `retry_class_mapping`） | ✅ 维持 |
| T2 | 重试循环 `Permanent` 不重试（计数==1） | none | **covered** — `runtime_loop.rs:797` `HttpStatus 401`（Permanent）→ 断言 `call_count==1` 且终态 `StopReason::Error` | ✅ 关闭 |
| T3 | 重试循环 `Transient` 指数退避 | covered | 维持（`test_retry`） | ✅ 维持 |
| T4 | `RateLimited` 用 `retry_after` 作延迟且封顶 | none | **partial** — `runtime_loop.rs:836` 覆盖「封顶」，但「用 retry_after 作延迟」不可判别，且「retry_after 缺失回退指数」缺失（见问题 1） | ❌ 未关闭 |
| T5 | adapter e2e（wiremock）429 + `Retry-After: 5`；401 分类断言 | covered | 维持（tests/adapters.rs） | ✅ 维持 |
| T6 | 退避等待可取消 | none | **covered** — `runtime_loop.rs:874` 用默认 `retry_base_delay=500ms` 触发退避 → `shutdown()` 打断；断言 `elapsed<100ms`、`call_count==1`、终态 `Aborted` | ✅ 关闭 |

## 问题（阻塞）

1. [Critical] **T4 未完全关闭：`RateLimited` 的「用 `retry_after` 作延迟」与「缺失回退指数」均未被真正护栏。** `tests/runtime_loop.rs:836-870`
   - (a) **缺「`retry_after` 缺失 → 回退指数退避」子用例。** 冻结节 T4 required case 第二段（042-review-r1.md:56）与规格 §2（042-provider-error-retry-class.md:52「`retry_after` 缺失时回退到指数退避」）均要求覆盖；全仓 `grep` 确认 `tests/` 无任何 `429 + retry_after: None` 经 runtime 退避的用例（adapters.rs:296 的 `retry_after: None` 是 401 用例）。
   - (b) **现有 cap 用例无法判别「实现了 retry_after」还是「忽略 retry_after」。** 用例设 `retry_after=Some(80ms)`（L842）而 `retry_max_delay=10ms`（L851），两条实现路径的延迟均为 `min(...)`→`10ms`：
     - 正确：`Some(80ms).unwrap_or(exponential).min(10ms) = 10ms`
     - 缺陷（若实现改为忽略 `retry_after`、恒用 `exponential`）：`min(500ms, 10ms) = 10ms`
     二者 `elapsed` 断言（L862-869）均通过 → 该用例只护栏了「封顶」，**未护栏 AC「`RateLimited` 用 `retry_after` 作为延迟」**（042-provider-error-retry-class.md:75）。`test_rate_limited_retry_after_is_capped` 函数名/文档（L834）声称验证「Retry-After 应作为等待时间」，但数值设计使其无法做到。存在「fake green」风险：runtime 静默忽略 `Retry-After` 时全绿。
   - 影响: 本任务核心特性「运行时尊重 `Retry-After`」在 runtime 层无判别性回归护栏；封顶逻辑有护栏，回退路径无护栏。
   - 建议（仅补测试，无需改生产代码）:
     - (i) 增补「`retry_after` 被使用」的判别用例：令 `retry_after` **显著小于** `retry_max_delay`（如 `Some(20ms)`、`retry_max_delay=1s`、默认 `retry_base_delay=500ms`）→ 断言 `elapsed` 明显接近 `retry_after`（远小于 cap，例如 `< 200ms`）。若实现忽略 `retry_after`，延迟为 `500ms`，用例即失败。
     - (ii) 增补「`retry_after` 缺失回退」用例：`HttpStatus{status:429, retry_after:None}` → 断言仍重试（`call_count==2`）且延迟回退 exponential（可用小 `retry_max_delay` 使断言稳定）。

## 建议（非阻塞）

1. [Note] `tests/runtime_loop.rs` 现 1290 行（>conventions 400 行上限）。属既有超标（本轮由此前 1149 行增加 141 行），非本候选引入的回归，不阻塞；建议后续按场景（重试 / 长度截断 / steering 等）拆分子模块。
2. [Note] 承接 r1/r2 非阻塞项：`parse_retry_after` 在 openai/anthropic adapter 逐字重复（建议抽共享单点）；`turn.rs:62-64` 全限定路径建议 `use` 导入 `RetryClass`。非强制。

## Closure Matrix（延续 r1 冻结，本轮未新增阻塞项）

本轮仅核验 r1 冻结矩阵：T2、T6 已关闭；T4 仍 open（问题 1 直接引用冻结 T4 required case 与规格 AC，未新增范围）。

## 结论
- [ ] 通过
- [x] 打回

## 下一步
Developer 需补齐冻结矩阵 **T4**（问题 1 的 (a)(b) 两点，仅 `tests/runtime_loop.rs` 新增用例，**无需改生产代码**）。T2/T6 已达标，不得回退。跑四门禁后重新提交并请复审。
