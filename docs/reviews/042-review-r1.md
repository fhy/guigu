# Task 042 Review - Round 1

## 基本信息
- 审查时间: 2026-09-22 18:32
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/042-provider-error-retry-class.md
- 候选提交: 29e08f1 `feat: classify provider errors for retries`

## 门禁结果
- cargo check: ✓ (`cargo check --all-targets`)
- cargo clippy: ✓ (`cargo clippy --all-targets --all-features -- -D warnings`，0 warning)
- cargo test: ✓ (`cargo test --all-targets`，0 失败；380 单元 + 各集成目标)
- cargo fmt: ✓ (`cargo fmt --check`)

## 代码审查

### 实现正确性（非阻塞确认）
- `RetryClass` 枚举 + 文档注释（src/core/provider.rs:55-69）符合规格 §1。
- `retry_class()` 映射（provider.rs:117-133）逐条核对规格表：`Network`/`Timeout`/`Request`→Transient；`Aborted`/`Parse`/`Build`→Permanent；`429`→RateLimited；`500..=599`→Transient；`400..=499`→Permanent；其余→Transient。与 AC 映射一致。
- `HttpStatus.retry_after` 为 additive 字段（provider.rs:96-101），`retry_after()`（provider.rs:136-145）仅 429 返回 `Some`，符合规格。
- 重试循环（src/core/runtime/turn.rs:58-86）先判 `Permanent` 直接传播，再判取消，再指数退避；`RateLimited` 用 `e.retry_after().unwrap_or(exponential).min(retry_max_delay)`（默认 30s 封顶，符合规格 §2「封顶」）。退避 select! 仍纳入 `signal.cancelled()`（turn.rs:80-83），003 纪律未破。
- adapter 解析 `Retry-After`（秒数或 HTTP-date）并填入字段（openai/mod.rs:70-102、anthropic/mod.rs:82-115），逻辑正确。
- 产品代码无 `unwrap()`；`HttpStatus` 构造/match 穷尽点已全部更新（仅 tests/adapters.rs:289 加 `..`）。

### 问题（阻塞）

1. [Critical] **规格 AC 要求的测试基本缺失，新增逻辑无任何测试覆盖。** 本提交 9 个文件中**未新增/修改任何测试断言**（仅 tests/adapters.rs:289 因 additive 字段加 `..`）。全仓 `grep` 确认：`tests/` 内 **0 处** 出现 `retry_class` / `retry_after` / `RetryClass` / `429` / `Retry-After`；`src/core/provider.rs` 无 `#[cfg(test)]`。
   - 违反规格 AC（docs/tasks/042-provider-error-retry-class.md:74-77）：
     - [ ] `retry_class` 映射表逐条单测（Network/Request/Timeout→Transient；Aborted/Parse/Build/401/400→Permanent；429→RateLimited；5xx→Transient）——**缺失**
     - [ ] 重试循环：`Permanent`/`Aborted` 不重试（计数 0）；`RateLimited` 用 `retry_after` 作为延迟且封顶——`Aborted` 部分由既有 test_stream_establishment_cancel 覆盖，**但 `Permanent`（非 Aborted，如 401/Parse）与 `RateLimited` 路径无测试**
     - [ ] adapter 端到端（wiremock）：429 + `Retry-After: 5` → `HttpStatus { retry_after: Some(5s) }`；401 → `HttpStatus`（分类 Permanent）——**429 用例缺失**（401 用例存在但未断言分类）
     - [ ] 退避等待可取消（`signal.cancel()` 打断退避）——**缺失**（既有测试仅覆盖建流期取消，非退避期取消）
   - 影响: `parse_retry_after`（两个 adapter 的新逻辑）、`retry_class` 边界（429/4xx/5xx/非 2xx）、RateLimited 封顶与退避可取消均无回归护栏；「fake green」风险——门禁全绿但新功能未被真实执行。
   - 建议: 补齐上述 4 项测试（见下方 closure matrix）。

### 建议（非阻塞）

1. [Warning] src/adapters/openai/mod.rs:95-102 与 src/adapters/anthropic/mod.rs:108-115 — `parse_retry_after` 逐字重复两份。
   - 影响: 同一逻辑双份维护，未来改动易漂移。
   - 建议: 抽到 `providers-http` 共享模块（如 `src/adapters/retry_after.rs` 或 core/http 工具）单点实现。

2. [Note] src/core/runtime/turn.rs:62-67 使用完全限定路径 `crate::core::provider::RetryClass::Permanent`，而 `ProviderError` 已从 provider 导入。
   - 建议: 在 turn.rs 顶部 `use` 中一并导入 `RetryClass`，与既有风格一致。

3. [Note] src/core/runtime/turn.rs:62-70 顺序变更引入一处边界行为差异：当 `signal.is_cancelled()` 且 provider 返回**非 Aborted 的 Permanent 错误**时，现返回该 Permanent 错误；pre-042 代码（`matches!(e, Aborted) || signal.is_cancelled()`）会统一返回 `Aborted`。因 `Aborted` 本身即 Permanent，abort 路径未回归，故不阻塞；请确认此差异符合 040/041 的 abort 可达性契约（如需保持「取消优先」语义可先判 `signal.is_cancelled()`）。

4. [Note - 流程] 本提交含 `docs/HISTORY.md`、`docs/tasks/042-provider-error-retry-class.md` 两个 `docs/` 文件（内容为 Architect 署名的排程/规格 v1.1 记录）。按 conventions「Developer MUST NOT write docs」属跨目录提交，请 PM 知悉（若为 Architect 已写好的记录由 Developer 顺手提交，建议后续由 Architect 自行提交）。

## Closure Matrix（冻结 — 后续轮次只核验本矩阵）

| # | Requirement (规格 AC) | 现状 Evidence level | 本轮 Required case |
|---|-----------------------|--------------------|--------------------|
| T1 | `retry_class` 映射表逐条单测 | **none** | 单测覆盖 Network/Request/Timeout→Transient；Aborted/Parse/Build→Permanent；400/401→Permanent；429→RateLimited；500/503→Transient |
| T2 | 重试循环 `Permanent` 不重试（计数 0） | **none**（Aborted 由既有测试覆盖） | 非 Aborted 的 Permanent（如 HttpStatus 401 或 Parse）→ provider.stream 调用计数 == 1，run 产出 Error 终态 |
| T3 | 重试循环 `Transient` 指数退避（计数可断言） | **covered**（既有 test_retry） | 维持既有覆盖，不改动 |
| T4 | `RateLimited` 用 `retry_after` 作延迟且封顶 | **none** | 429 带 `retry_after` → 延迟 = `min(retry_after, retry_max_delay)`；retry_after 缺失 → 回退指数退避 |
| T5 | adapter e2e（wiremock）429 + `Retry-After: 5` | **none** | wiremock 返回 429 + `Retry-After: 5` → `HttpStatus { retry_after: Some(5s) }`；401 用例补断言 `retry_class()==Permanent` |
| T6 | 退避等待可取消 | **none** | `signal.cancel()` 在退避等待期间触发 → 立即返回 `Aborted`，不等满延迟 |

## 结论
- [ ] 通过
- [x] 打回

## 下一步
Developer 需在 `tests/`（及必要时 provider.rs 的 `#[cfg(test)]`）补齐闭环矩阵 T1、T2、T4、T5、T6 项测试（T3 已覆盖）。跑四门禁后重新提交并请复审。非阻塞建议 1-4 可一并处理但非强制。
