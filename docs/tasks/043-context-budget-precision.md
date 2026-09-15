# Task 043: 上下文预算精确化（实际 usage + 预留输出）

## Background

reviewer 次要优化项：`src/core/context.rs` 的预算只计算消息正文，未计入 system prompt、工具 schema、协议包装，也未预留输出 token。与 pi 的 `contextWindow - reserveTokens` 相比，容易**过晚压缩**，导致本可避免的截断/压缩。

## Goal

- 预算计算优先使用 provider 返回的**实际 usage**，再对新增消息做增量估算。
- 预留输出空间，并把固定开销（system prompt + 工具 schema + 协议包装）计入预算。

## Design Notes

### 1. 预算公式（core/context.rs）

```
effective_budget = context_window - reserve_output_tokens
fixed_overhead   = estimate_tokens(system_prompt)
                 + estimate_tokens(serialize(tool_schemas))
                 + protocol_wrapper_tokens          // 常量，如 128
available        = effective_budget - fixed_overhead
```

- `reserve_output_tokens`：新增预算配置字段，默认给一个保守值（如 1024）；CLI 可按模型调整。
- `protocol_wrapper_tokens`：常量，覆盖 provider 请求体骨架 + 消息 role/type 包装的固定 token 开销。
- 压缩/截断触发条件由「消息正文估算 > context_window」改为「消息估算 > available」。

### 2. 消息 token 估算：实际 usage 优先

- `AssistantMessage.usage` 已携带 `Usage { input, output, .. }`（002/007 定稿）。维护「最近一次已知 input token 基线」：
  - 取 transcript 中**最后一条** `AssistantMessage.usage.input` 作为基线（它代表了当时完整上下文的实际输入 token）。
  - 对基线之后**新增**的消息（那条 assistant 之后的 User/Assistant/ToolResult），用启发式（`chars/4`）增量估算。
  - 当前估算 = 基线 + 新增消息估算。
- 无任何 usage 时，回退到全量 `chars/4` 估算（现状行为）。
- `estimate_tokens` 启发式保持 `chars/4`（粗估，与 003/008 一致），不引入 tokenizer。

### 3. 配置字段

`CompactionPolicy`（008 定稿）新增两字段（additive）：

```rust
pub struct CompactionPolicy {
    pub budget_tokens: usize,        // 既有：触发压缩的阈值（语义更新为「消息可用预算」）
    pub keep_recent: usize,          // 既有：保留最近 N turn（041 升级）
    pub reserve_output_tokens: usize,   // 新增：预留输出
    pub protocol_wrapper_tokens: usize, // 新增：协议包装固定开销
}
```

- `Default` 保守：`reserve_output_tokens` 与 `protocol_wrapper_tokens` 取合理默认，保证等价「几乎不压缩」。
- `budget_tokens` 语义说明：本任务后 `budget_tokens` 表示「消息正文可用 token」，总窗口 = `budget_tokens + reserve_output_tokens + fixed_overhead`。

### 4. 与 041 的边界

- 本任务只改「何时触发压缩/截断」（预算计算更精确），不改「如何提交/截断」（041 已定）。
- 依赖顺序：043 在 041 之后实现（都动 `context.rs`/`CompactionPolicy`，避免冲突）；若同批，Developer 需合并两任务的 `CompactionPolicy` 改动。

## Files

- src/core/context.rs（预算公式 + 实际 usage 基线估算 + 单测）
- src/core/compactor.rs（`CompactionPolicy` 增 `reserve_output_tokens`/`protocol_wrapper_tokens`）
- src/core/runtime/（把 `context_window` 与 policy 传入预算计算）
- tests/compactor.rs（预算触发回归测试）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets --all-features -D warnings passes
- [ ] cargo test --all-targets passes
- [ ] cargo fmt --check passes
- [ ] 预算公式单测：`available = context_window - reserve_output - (system+tools+protocol)`；超 `available` 触发压缩
- [ ] 实际 usage 基线：构造含 `usage.input` 的 transcript → 估算 = 基线 + 新增消息估算；无 usage → 全量 `chars/4` 回退
- [ ] `reserve_output_tokens`/`protocol_wrapper_tokens` 计入预算（边界断言：空/短/长文本）
- [ ] 产品代码无 `unwrap()`；更新所有 `CompactionPolicy` 构造点
- [ ] 单文件 ≤ 400 行，超则拆子模块并记录

## 修订记录

- v1.0（2026-09-13，Architect）：初稿。依据维护审查次要项 #1：预算改为 `context_window - reserve_output_tokens` 再扣固定开销（system/tools/protocol）；消息估算优先用最后一条 `AssistantMessage.usage.input` 作基线 + 新增消息增量估算，无 usage 回退 `chars/4`；`CompactionPolicy` 增两字段（additive）。
