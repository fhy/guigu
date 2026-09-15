# Task 040: Runtime 正确性修复 — tool_call 截断保护 + 建流取消/超时

## Background

reviewer 在维护审查中发现两个高优先级运行时缺陷：

1. **输出截断仍执行工具调用**：`src/core/runtime/mod.rs` 在收集 assistant 响应后只判断「是否存在 tool_calls」，未检查 `AssistantMessage.stop_reason == Length`。当模型在工具参数中途达到 token 上限，截断后的参数若碰巧仍是合法 JSON，就会执行**不完整的 bash / write / edit** 操作。pi 对此有明确保护：`stopReason === "length"` 时把整批工具调用标记失败，不执行。
2. **建流阶段不可可靠取消**：`src/core/runtime/turn.rs` 直接 `provider.stream(...).await`，取消信号只参与重试等待与已取得 stream 之后的 `select!`。若自定义 provider 或 HTTP 建流阶段一直挂起，`abort`/`shutdown`/TUI 退出都会一直等待。

## Goal

- 在进入 tool 执行步骤前，若 assistant 消息 `stop_reason == Length` 且含 `ToolCall` 段，则**不执行任何工具**，将整批标记为失败（合成错误 ToolResult 注入上下文），不落盘真实副作用。
- 把每次 `provider.stream()` 建流也纳入 `tokio::select!` 的取消与可配置超时分支。

## Design Notes

### 1. StopReason::Length 工具调用保护（core/runtime，tool 编排入口）

- 判定条件：`assistant.stop_reason == Some(StopReason::Length)` **且** `assistant.content` 含至少一个 `AssistantContent::ToolCall`。
- 命中时：
  - **不调用**任何 `tool.execute()`。
  - 对每个待执行的 ToolCall，按顺序发出生命周期事件：`ToolExecutionStart { tool_call_id, tool_name, args }` → `ToolExecutionEnd { tool_call_id, tool_name, result, is_error: true }`，其中 `result` 为合成 ToolResult：
    - `content: vec![ToolResultContent::Text("tool call arguments truncated by length limit".into())]`
    - `is_error: true`、`details: None`
  - 将每个合成 ToolResult 构造为 `ToolResultMessage { tool_call_id, tool_name, is_error: true, content: ..., details: None }` 追加进 transcript（供下一轮模型看到失败，自行纠正或继续）。
  - 本 turn 正常结束（不中断 run，不产生 `Aborted`）。
- 未命中（`stop_reason` 非 Length，或无 ToolCall）→ 走既有 tool 执行路径，行为不变。
- 边界：`stop_reason == Length` 且**无** ToolCall 时无需保护（仅截断的文本消息，合法终态）。

### 2. 建流取消 + 可配置请求超时（core/runtime/turn.rs + core/provider.rs）

`turn.rs` 用 `select!` 包裹每次 `provider.stream(request.clone())`：

```rust
let stream = match request_timeout {
    Some(d) => tokio::select! {
        r = provider.stream(request.clone()) => r,
        _ = request.signal.cancelled() => Err(ProviderError::Aborted),
        _ = tokio::time::sleep(d)       => Err(ProviderError::Timeout),
    },
    None => tokio::select! {
        r = provider.stream(request.clone()) => r,
        _ = request.signal.cancelled() => Err(ProviderError::Aborted),
    },
};
```

- `ProviderError` 新增两个变体（**additive，不破坏既有变体与构造器**）：
  ```rust
  #[error("provider request aborted")]
  Aborted,
  #[error("provider request timed out")]
  Timeout,
  ```
- `request_timeout` 可配置：在 `LoopConfig` 增 `pub request_timeout: Option<Duration>`；默认 `None`（保持既有语义，零行为变化），CLI/TUI 装配处可设非空值（如 30s）。
- `request.signal` 即 `ProviderRequest.signal`（003 定稿），turn.rs 内可直接取用，无需改 `ModelProvider` 签名。
- 重试语义：`Aborted` **不重试**、立即向上传播（对应 abort 终态）；`Timeout` 视为可重试（详见 Task 042 重试分类）。本任务至少保证 `Aborted` 不进入指数退避重试。

## Files

- src/core/runtime/mod.rs（tool 编排入口加 Length 保护）
- src/core/runtime/turn.rs（建流 select! + 超时）
- src/core/runtime/tools.rs（若 tool 编排逻辑落在此处，则改此处）
- src/core/provider.rs（`ProviderError` 增 `Aborted`/`Timeout`）
- src/core/runtime.rs 或 LoopConfig 定义处（增 `request_timeout` 字段）
- tests/runtime_loop.rs（回归测试）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets --all-features -D warnings passes
- [ ] cargo test --all-targets passes
- [ ] cargo fmt --check passes
- [ ] Length 保护单测：fake provider 回放 `ToolCallStart/Delta/End` + `Done{ stop_reason: Length }` → 断言**无**任何 `tool.execute()` 被调用；断言每个 tool_call 产出 `ToolExecutionEnd{ is_error: true }` + 合成 ToolResult 已入 transcript
- [ ] Length 保护负例：`stop_reason == Completed` 且含 ToolCall → 正常执行工具（行为不变）
- [ ] Length 且无 ToolCall → 正常结束，无合成 ToolResult
- [ ] 建流取消：自定义 provider 的 `stream()` 挂起（用 `pending()` future）→ `signal.cancel()` 后 `stream()` 返回 `ProviderError::Aborted`，且**不进入重试**、run 产出 `Aborted` 终态
- [ ] 建流超时：`request_timeout = Some(small)` + 挂起 provider → 返回 `ProviderError::Timeout`；`None` 时无超时分支
- [ ] 产品代码无 `unwrap()`；异步测试用 `tokio::test`；更新所有 `ProviderError` 的 `match` 穷尽点
- [ ] 单文件 ≤ 400 行，超则拆子模块并记录

## 修订记录

- v1.0（2026-09-13，Architect）：初稿。依据维护审查问题 #1/#3：tool_call 截断保护采用 pi 的「`stopReason === length` 整批失败不执行」语义，合成错误 ToolResult 注入上下文；建流阶段纳入 `select!` 取消 + 可配置 `LoopConfig::request_timeout`，`ProviderError` 增 `Aborted`/`Timeout`（additive）。
