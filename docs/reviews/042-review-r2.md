# Task 042 Review - Round 2

## 基本信息
- 审查时间: 2026-09-22 19:05
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/042-provider-error-retry-class.md
- 候选提交: 9d39f4c `fix(provider): add retry classification regression tests`
- 上轮报告: docs/reviews/042-review-r1.md（round 1 打回）

## 门禁结果
- cargo check: ✓ (`cargo check --all-targets`)
- cargo clippy: ✓ (`cargo clippy --all-targets --all-features -- -D warnings`，0 warning)
- cargo test: ✓ (`cargo test --all-targets`，573 passed / 0 failed；含新增 3 个用例)
- cargo fmt: ✓ (`cargo fmt --check`)

本轮新增测试均真实执行通过：
- `core::provider::tests::retry_class_mapping` ✓
- `core::provider::tests::retry_after_only_applies_to_429` ✓
- `openai_http_429_parses_retry_after` ✓（wiremock 429 + `Retry-After: 5`）
- `openai_http_401_returns_http_status_error` ✓（补 401 分类断言）

## 代码审查（对照冻结 Closure Matrix）

| # | Requirement (规格 AC) | 上轮 | 本轮 Evidence | 判定 |
|---|-----------------------|------|---------------|------|
| T1 | `retry_class` 映射表逐条单测 | none | **covered** — provider.rs:113-166 `retry_class_mapping` 覆盖 Network/Request/Timeout→Transient；Aborted/Parse/Build→Permanent；400/401→Permanent；429→RateLimited；500/503→Transient | ✅ 关闭 |
| T2 | 重试循环 `Permanent` 不重试（计数==1） | none | **none** — `tests/` 无任何非 Aborted 的 Permanent（如 `HttpStatus 401` / `Parse`）驱动 runtime 的用例；`FakeProvider` 仅能返回 `Request`（Transient） | ❌ 未关闭 |
| T3 | 重试循环 `Transient` 指数退避（计数可断言） | covered | **covered** — `tests/runtime_loop.rs:752 test_retry`（未改动） | ✅ 维持 |
| T4 | `RateLimited` 用 `retry_after` 作延迟且封顶 | none | **none** — 无 429 经 runtime 退避路径的用例；`min(retry_after, retry_max_delay)` 与「retry_after 缺失回退指数」均无断言 | ❌ 未关闭 |
| T5 | adapter e2e（wiremock）429 + `Retry-After: 5`；401 补分类断言 | none | **covered** — tests/adapters.rs:308-338（429→`Some(5s)`）；tests/adapters.rs:293-300（401→`retry_class()==Permanent`） | ✅ 关闭 |
| T6 | 退避等待可取消（`signal.cancel()` 打断退避） | none | **none** — 既有 `test_stream_establishment_cancel`（runtime_loop.rs:994）仅覆盖**建流期**取消，非退避等待期取消 | ❌ 未关闭 |

### 问题（阻塞）

1. [Critical] **T2 未关闭：`Permanent`（非 Aborted）不重试路径无回归护栏。**
   - 证据: `grep -rn "RetryClass\|Permanent\|429\|retry_max_delay" tests/` 仅命中 `tests/adapters.rs:299`（401 分类）。`src/core/runtime/turn.rs:62-67` 新增的 `Permanent → return Err(e)` 分支在生产路径上无任何测试驱动。
   - 违反规格 AC（042-provider-error-retry-class.md:75）：「`Permanent`/`Aborted` 不重试（计数 0）」
   - 影响: 若该分支被误改（例如顺序倒置导致先退避再判 Permanent），无护栏拦截。`FakeProvider` 需扩展为可返回 `HttpStatus`/`Parse` 以驱动该路径。
   - 建议: 新增 runtime 用例——provider 返回非 Aborted Permanent（如 `HttpStatus{status:401}` 或 `Parse`）→ 断言 `call_count == 1` 且终态 `stop_reason == Error`。

2. [Critical] **T4 未关闭：`RateLimited` 延迟与封顶无回归护栏。**
   - 证据: `tests/` 无 429 进入 runtime 重试循环的用例；`turn.rs:76-79` 的 `retry_after().unwrap_or(exponential).min(retry_max_delay)` 未被任何断言覆盖。
   - 违反规格 AC（042:75）：「`RateLimited` 用 `retry_after` 作为延迟且封顶」；规格 §2 明确「封顶（如 ≤ 30s）；`retry_after` 缺失时回退到指数退避」。
   - 影响: 封顶逻辑与缺失回退路径无护栏，「fake green」风险。
   - 建议: 用可控时钟或小 `retry_max_delay` 断言：429 带大 `retry_after` → 实际延迟 = `retry_max_delay`（封顶）；429 无 `retry_after` → 回退指数退避且可重试计数可断言。

3. [Critical] **T6 未关闭：退避等待期取消无回归护栏。**
   - 证据: 既有取消用例（runtime_loop.rs:994）覆盖的是**建流期** `signal.cancelled()`，不覆盖 `turn.rs:80-83` 退避 `select!` 中的取消分支。
   - 违反规格 AC（042:77）：「退避等待可取消（`signal.cancel()` 打断退避）」。
   - 影响: T4 引入的退避路径（尤其 `retry_after` 可能长达数十秒）若不可取消，将导致 abort 响应延迟；无护栏。
   - 建议: provider 先返回一次可重试错误（触发退避）→ 在退避等待期 `handle.shutdown()`/`abort()` → 断言立即（远早于退避延迟）返回 `Aborted`，且 `call_count == 1`。

### 建议（非阻塞，承接 r1，本轮未处理不阻塞）

1. [Warning] `parse_retry_after` 在 openai/mod.rs 与 anthropic/mod.rs 逐字重复 → 建议抽到共享模块单点实现。
2. [Note] `src/core/runtime/turn.rs:62-67` 使用完全限定路径 `crate::core::provider::RetryClass::Permanent` → 建议在顶部 `use` 一并导入 `RetryClass`。
3. [Note] `turn.rs:62-70` 顺序变更（先判 Permanent 后判 `signal.is_cancelled()`）使「取消 + 非 Aborted Permanent」返回该 Permanent 错误，非 `Aborted`。因 Aborted 本身即 Permanent，abort 路径未回归，不阻塞；请确认符合 040/041 abort 可达性契约。

## Closure Matrix（延续 r1 冻结，未新增阻塞项）

本轮仅核验 r1 冻结矩阵。T1、T5 已关闭；T2、T4、T6 仍 open，构成打回依据（均直接引用规格 AC 与 r1 `Required case`，未新增范围）。

## 结论
- [ ] 通过
- [x] 打回

## 下一步
Developer 需补齐闭环矩阵 **T2、T4、T6** 三项（T1/T3/T5 已达标，不得回退）。**无需改动生产代码**，仅补充 `tests/runtime_loop.rs` 用例（必要时扩展 `FakeProvider` 以返回 `HttpStatus`/`Parse`/`RateLimited`）。跑四门禁后重新提交并请复审。非阻塞建议 1-3 可一并处理但非强制。
