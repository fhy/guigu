# Task 042 Review - Round 4

## 基本信息
- 审查时间: 2026-09-22 19:15
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/042-provider-error-retry-class.md
- 候选提交: 330ca98 `test(runtime): cover rate limit delay fallback`
- 上轮报告: docs/reviews/042-review-r3.md（round 3 打回，仅剩 T4）

## 门禁结果
- cargo check: ✓ (`cargo check --all-targets`)
- cargo clippy: ✓ (`cargo clippy --all-targets --all-features -- -D warnings`，0 warning)
- cargo test: ✓ (`cargo test --all-targets`，0 failed；runtime_loop 目标 21 passed)
- cargo fmt: ✓ (`cargo fmt --check`)

本轮新增 2 个 runtime 用例均真实执行通过：
- `test_rate_limited_retry_after_is_used` ✓
- `test_rate_limited_without_retry_after_uses_exponential_backoff` ✓

## 代码审查（对照冻结 Closure Matrix）

| # | Requirement | 上轮 | 本轮 Evidence | 判定 |
|---|-------------|------|---------------|------|
| T1 | `retry_class` 映射表逐条单测 | covered | 维持（provider.rs `retry_class_mapping`） | ✅ 维持 |
| T2 | 重试循环 `Permanent` 不重试（计数==1） | covered | 维持（`runtime_loop.rs:797`，401→Permanent→`call_count==1`+`StopReason::Error`） | ✅ 维持 |
| T3 | 重试循环 `Transient` 指数退避 | covered | 维持（`test_retry`） | ✅ 维持 |
| T4 | `RateLimited` 用 `retry_after` 作延迟且封顶 / 缺失回退指数 | partial | **covered** — 见下方判别性分析（`is_capped` + `is_used` + `without_retry_after` 三例闭环） | ✅ 关闭 |
| T5 | adapter e2e（wiremock）429 + `Retry-After: 5`；401 分类断言 | covered | 维持（tests/adapters.rs） | ✅ 维持 |
| T6 | 退避等待期可取消 | covered | 维持（`runtime_loop.rs:948`，`shutdown()` 打断退避，`elapsed<100ms`+`call_count==1`+`Aborted`） | ✅ 维持 |

### T4 判别性分析（r3 问题 1 (a)(b) 核验）

**(b) 「`retry_after` 被真正使用」— `test_rate_limited_retry_after_is_used`（runtime_loop.rs:872-908）**
- 参数：`retry_after=Some(20ms)`、`retry_max_delay=1s`、`retry_base_delay=默认 500ms`。
- 正确实现：`Some(20ms).unwrap_or(exponential).min(1s) = 20ms` → `elapsed≈20ms`，满足 `>=15ms` 且 `<200ms`。
- 缺陷实现（忽略 `retry_after`，恒用 exponential）：`min(500ms·2⁰, 1s) = 500ms` → 违反 `<200ms`，**用例失败**。
- 结论：该断言对「是否使用 `retry_after`」具判别性，r3 问题 1(b) 关闭。阈值（15ms/200ms）与两条路径（20ms vs 500ms）间距充足，无竞态风险。

**(a) 「`retry_after` 缺失 → 回退指数退避」— `test_rate_limited_without_retry_after_uses_exponential_backoff`（runtime_loop.rs:910-944）**
- 参数：`HttpStatus{status:429, retry_after:None}`、`retry_base_delay=10ms`、`retry_max_delay=50ms`。
- 正确实现：`None.unwrap_or(10ms·2⁰).min(50ms) = 10ms` → `call_count==2` 且 `elapsed≈10ms`。
- 缺陷实现（None 即不重试/提前返回）：`call_count==1` → 违反断言，**用例失败**。
- 结论：覆盖规格 §2「`retry_after` 缺失时回退到指数退避」，r3 问题 1(a) 关闭。

三例（`is_capped` 封顶 + `is_used` 使用 + `without_retry_after` 回退）共同闭环规格 AC「`RateLimited` 用 `retry_after` 作为延迟且封顶」。判据均直接引用 042-review-r1.md:56 冻结 T4 required case 与规格 AC（042:75），未新增范围。

## 建议（非阻塞，承接 r1/r2/r3，均不影响判定）

1. [Warning] `parse_retry_after` 在 `src/adapters/openai/mod.rs` 与 `src/adapters/anthropic/mod.rs` 逐字重复 → 建议抽到共享模块（`src/adapters/retry_after.rs` 或 core/http 工具）单点实现。
2. [Note] `src/core/runtime/turn.rs:62-64` 使用完全限定路径 `crate::core::provider::RetryClass::Permanent` → 建议在顶部 `use` 一并导入 `RetryClass`。
3. [Note] `tests/runtime_loop.rs` 现 1364 行（>conventions 400 行上限）。属既有超标（本轮 +74 行），非本候选引入的回归，不阻塞；建议后续按场景（重试 / 长度截断 / steering 等）拆分子模块。
4. [Note-流程] 承接 r1：本任务此前的 `docs/HISTORY.md` / 规格 v1.1 由 Developer 随手提交属跨目录提交，请 PM 知悉。

## Closure Matrix（延续 r1 冻结，本轮新增 0 个阻塞项）

本轮核验 r1 冻结矩阵：T4 关闭，至此 **T1–T6 全部关闭**。无新增阻塞项。

## 结论
- [x] 通过
- [ ] 打回

## 下一步
所有冻结矩阵项达标，门禁四绿。Task 042 通过，回复 PM 进入下一阶段。
