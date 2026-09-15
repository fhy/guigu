# Task 041: 上下文安全 — 压缩提交语义 + 拓扑安全截断

## Background

reviewer 发现两个高优先级上下文缺陷：

1. **压缩被取消/失败会永久删除历史**：`src/core/context.rs` 把包括 `Cancelled` 在内的所有压缩错误统一降级为 `keep.to_vec()`，随后 `src/core/runtime/mod.rs` 把结果直接写回权威 transcript。用户在压缩期间 `abort`、或网络错误/空摘要等压缩失败，都会导致旧消息永久消失。
2. **截断破坏工具消息拓扑**：`src/core/context.rs` 按单条消息从头删除，截断点可能落在 `Assistant(ToolCall) | ToolResult` 之间，最终请求以孤立的 `ToolResult` 开头，OpenAI/Anthropic 会拒绝没有对应 tool call 的 result。

## Goal

- 压缩失败（含取消）**不再改写权威 transcript**；只有成功生成摘要后才提交 transcript/session 变更。
- 截断按「完整 turn / user boundary」选择切点，保证 tool call/result 成组保留，绝不产出孤立 ToolResult。

## Design Notes

### 1. 拆分「请求投影」与「提交」（核心决策）

把 008 的 `prepare_context` 编排从「返回消息列表并直接改写 transcript」重构为「只读投影 + 可选的提交计划」：

```rust
pub struct PreparedContext {
    pub request_messages: Vec<Arc<Message>>, // 本轮发送给 provider 的消息（投影）
    pub commit: Option<CompactionCommit>,    // 仅在压缩成功时 Some
}

pub struct CompactionCommit {
    pub summary: String,      // 摘要文本
    pub keep_from: usize,     // 提交 = 用 [User(summary)] 替换 transcript[0..keep_from]
}
```

每轮编排流程（落 `context.rs`，由 runtime 调用）：

```
plan_context(transcript, policy, compactor, signal) -> Result<PreparedContext, Cancelled>:
    if estimate_tokens(transcript) <= budget:
        return Ok(PreparedContext { request_messages: transcript.clone(), commit: None })
    // 超预算 → 尝试压缩旧消息
    match compactor.compact(CompactionRequest { messages: to_compact, signal }):
        Ok(summary) =>
            return Ok(PreparedContext {
                request_messages: [User(summary)] ++ keep,
                commit: Some(CompactionCommit { summary, keep_from: to_compact.len() }),
            })
        Err(CompactionError::Cancelled) =>
            return Err(Cancelled)            // 向上传播：终止 run，transcript 原样
        Err(_other) =>                        // Provider/EmptyInput/EmptySummary
            return Ok(PreparedContext {
                request_messages: truncate_to_budget(transcript), // 仅本次请求临时截断
                commit: None,                                      // 不改写 transcript
            })
```

**runtime 侧提交纪律（关键）**：

- `plan_context` 返回 `Err(Cancelled)` → runtime **停止循环**，产出 `stop_reason: Aborted` 终态，**transcript 不变**、不写 session。
- `Ok(PreparedContext { commit: Some(c), .. })` → 发送 `request_messages` 前，**提交** `c`：用 `[User(c.summary)]` 替换 `transcript[0..c.keep_from]`，并持久化到 session（JSONL）。
- `Ok(PreparedContext { commit: None, .. })` → 仅用 `request_messages` 做本次请求，transcript 与 session **均不写回**。

不变式：**只有压缩成功（`commit: Some`）才发生 transcript/session 变更**；取消与普通失败均不丢历史。

### 2. 拓扑安全截断（turn / user boundary）

- 定义「turn」= 一条 `User` 消息 + 其后的所有 `Assistant`/`ToolResult` 消息，直到下一条 `User`（不含）。
- 合法切点 = 某条消息为 `User`（turn 起点）。依赖 transcript 不变式：**首条必为 `User`**，且 `User` 只出现在 turn 起点（绝不落在 `Assistant(ToolCall)` 与其 `ToolResult` 之间）——该不变式由 append-only runtime 天然保证。
- `truncate_to_budget(messages, max_tokens)`：
  1. `estimate_tokens(messages) <= max_tokens` → 原样返回。
  2. 否则从头部**整 turn 丢弃**（切点推进到下一条 `User` 边界），直到满足预算。
  3. 若丢弃到只剩最后一个 turn 仍超预算 → 至少保留最后 1 个 turn，不产出空列表。
  4. 防御：若 `messages[0]` 非 `User`（不应发生），仍以首条为切点，但不得切断 tool call/result 成组（即切点前一条不得是含 `ToolCall` 的 `Assistant` 且切点后是 `ToolResult`）。
- 008 的 `CompactionPolicy.keep_recent` 语义**升级为 turn 粒度**：`keep_recent` 表示保留最近 N 个完整 turn（非 N 条消息）；`Default` 保持等价「几乎不压缩」的保守值。
- 该 `truncate_to_budget` 同时服务 §1 的「临时截断」路径与所有保守降级路径，单一实现、统一拓扑保证。

### 3. 与 008 契约的关系

- `Compactor` trait（`core/compactor.rs`）**签名不变**（仍产出 summary 或 error）。
- 变化仅限编排层：008 的「降级 = 丢弃旧消息」改为「降级 = 本次请求临时截断，不改写 transcript」；`CompactionError::Cancelled` 从「降级」改为「终止 run 且不丢历史」。
- `CompactionError` 本身无需改动；区分逻辑在编排层按变体分支。

## Files

- src/core/context.rs（`plan_context`/`PreparedContext`/`CompactionCommit`/`truncate_to_budget` + 单测）
- src/core/runtime/（主循环接入 plan_context，实施「仅在 commit: Some 时提交」纪律）
- src/core/compactor.rs（若需 expose `keep_recent` turn 语义，仅微调；trait 签名不变）
- tests/compactor.rs、tests/runtime_loop.rs（回归测试）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets --all-features -D warnings passes
- [ ] cargo test --all-targets passes
- [ ] cargo fmt --check passes
- [ ] 压缩成功：摘要注入请求、`commit: Some`、transcript 与 session 均被替换为 `[User(summary)] ++ keep`
- [ ] 压缩 `Cancelled`：`plan_context` 返回取消 → run 产出 `Aborted`，transcript/session **原样**（断言旧消息仍在，未丢）
- [ ] 压缩失败（Provider/EmptySummary/EmptyInput）：本次请求用截断投影，transcript/session **未写回**（断言完整 transcript 仍在内存）
- [ ] 拓扑安全：构造截断点落在 `Assistant(ToolCall)`/`ToolResult` 之间的 transcript，断言截断后请求**不以 ToolResult 开头**、tool call/result 成组保留
- [ ] `keep_recent` 按 turn 粒度：保留最近 N 个完整 turn
- [ ] 产品代码无 `unwrap()`；异步测试用 `tokio::test`
- [ ] 单文件 ≤ 400 行，超则拆子模块并记录

## 修订记录

- v1.0（2026-09-13，Architect）：初稿。依据维护审查问题 #2/#4：引入 `PreparedContext { request_messages, commit }` 把「请求投影」与「transcript 提交」解耦，提交仅在压缩成功时发生；`Cancelled` 终止 run 且不丢历史；截断升级为 turn/user boundary 粒度，保证 tool call/result 成组。`Compactor` trait 签名不变。
