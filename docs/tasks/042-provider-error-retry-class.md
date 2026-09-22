# Task 042: ProviderError 重试分类 + Retry-After

## Background

reviewer 次要优化项：当前 `ProviderError` 无重试分类，认证失败、参数错误等**永久错误**也会被 runtime 指数退避重试，浪费等待时间且掩盖真实错误。同时 429 限流未尊重 `Retry-After`。

## Goal

- 为 `ProviderError` 引入可判定重试类别：`transient` / `rate-limited` / `permanent`。
- runtime 重试循环只对可重试类别退避；`rate-limited` 尊重 `Retry-After`（封顶）。
- 与 Task 040 的 `Aborted`/`Timeout` 分类协同：`Aborted` 不重试、`Timeout` 可重试。

## Design Notes

### 1. 重试分类

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryClass {
    Transient,     // 网络瞬断 / 5xx / 超时 → 指数退避重试
    RateLimited,   // 429 → 可重试，尊重 Retry-After
    Permanent,     // 认证/参数/解析/构造 → 不重试
}
```

`ProviderError` 新增关联方法（**不改既有变体，仅 additive 字段**）：

```rust
impl ProviderError {
    pub fn retry_class(&self) -> RetryClass { /* 见下表 */ }
    pub fn retry_after(&self) -> Option<Duration> { /* 仅 RateLimited 返回 Some */ }
}
```

| ProviderError 变体 | 分类 |
|---|---|
| `Network(_)` | Transient |
| `Request(_)` | Transient（语义等同 `Network`：请求发送/传输层失败，可重试） |
| `Timeout`（040 新增） | Transient |
| `Aborted`（040 新增） | Permanent（且 runtime 必须立即传播，不重试） |
| `HttpStatus { status, .. }` | 401/403/400/404/422 → Permanent；429 → RateLimited；500..=599 → Transient；其余 4xx → Permanent；其余非 2xx → Transient |
| `Parse(_)` | Permanent |
| `Build(_)` | Permanent |

- `HttpStatus` 变体新增 `retry_after: Option<Duration>` 字段（additive）。adapter 解析响应头 `Retry-After`（秒数或 HTTP-date，用 `httpdate`/手写解析均可），填入该字段；缺省 `None`。

### 2. runtime 重试循环

- 重试前判定 `err.retry_class()`：
  - `Permanent` / `Aborted` → **不重试**，立即传播。
  - `Transient` → 既有指数退避（0.5s·2^n，上限、可取消）。
  - `RateLimited` → 延迟 = `retry_after().unwrap_or(base)`，**封顶**（如 ≤ 30s）；`retry_after` 缺失时回退到指数退避。
- 退避等待仍纳入 `signal.cancelled()` 的 `select!`（003 既有纪律不变）。

### 3. 与既有契约关系

- 003 定稿「仅重试 provider 请求，不重试工具」不变；本任务只细化「哪些 provider 错误值得重试、退避多久」。
- 007 定稿 `ProviderError` 语义（Network/HttpStatus/Parse/Build）不变，`retry_class` 是叠加在其上的纯函数，不改变体含义。实现中若存在 `Request(_)` 独立变体（请求发送/传输层失败），归 `Transient`，与 `Network` 同义（见映射表）。
- 040 的 `Aborted`/`Timeout` 需本任务在 `retry_class` 中给出分类（见上表），故 042 依赖 040 先落地（或同批实现，Developer 需保证两个任务的 `ProviderError` 改动合并一致）。

## Files

- src/core/provider.rs（`RetryClass` + `retry_class()`/`retry_after()` + `HttpStatus.retry_after` 字段）
- src/core/runtime/（重试循环接入分类 + Retry-After）
- src/adapters/openai.rs、src/adapters/anthropic.rs（解析 `Retry-After` 头并填 `retry_after`）
- tests/runtime_loop.rs、tests/adapters.rs（回归测试）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets --all-features -D warnings passes
- [ ] cargo test --all-targets passes
- [ ] cargo fmt --check passes
- [ ] `retry_class` 映射表逐条单测（Network/Request/Timeout→Transient；Aborted/Parse/Build/401/400→Permanent；429→RateLimited；5xx→Transient）
- [ ] 重试循环：`Permanent`/`Aborted` 不重试（计数 0）；`Transient` 指数退避重试（计数可断言）；`RateLimited` 用 `retry_after` 作为延迟且封顶
- [ ] adapter 端到端（wiremock）：429 + `Retry-After: 5` → `HttpStatus { retry_after: Some(5s) }`；401 → `HttpStatus`（分类 Permanent）
- [ ] 退避等待可取消（`signal.cancel()` 打断退避）
- [ ] 产品代码无 `unwrap()`；更新所有 `ProviderError` 构造/match 穷尽点
- [ ] 单文件 ≤ 400 行，超则拆子模块并记录

## 修订记录

- v1.0（2026-09-13，Architect）：初稿。依据维护审查次要项 #2：`ProviderError::retry_class()` 区分 transient/rate-limited/permanent，`HttpStatus` 增 `retry_after`（additive），runtime 只对可重试类别退避并尊重 Retry-After（封顶）；`Aborted`/`Timeout` 分类协同 040。
- v1.1（2026-09-19，Architect）：响应 Reviewer 规格未覆盖项——`ProviderError::Request(_)`（实现中独立变体，请求发送/传输层失败）明确归 `Transient`，映射表与 AC 单测补列。理由：语义等同 `Network`；既有重试契约已按可重试处理，042 不得改变行为。
