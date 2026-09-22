# Task 043: 上下文预算精确化（实际 usage + 预留输出）

## Background

reviewer 次要优化项：`src/core/context.rs` 的预算只计算消息正文，未计入 system prompt、工具 schema、协议包装，也未预留输出 token。与 pi 的 `contextWindow - reserveTokens` 相比，容易**过晚压缩**，导致本可避免的截断/压缩。

## Goal

- 预算计算优先使用 provider 返回的**实际 usage**，再对新增消息做增量估算。
- 预留输出空间，并把固定开销（system prompt + 工具 schema + 协议包装）计入预算。

## Design Notes

### 1. 预算公式（core/context.rs）

统一以「预计发往 provider 的**总输入 token**」为单一口径，入口函数 `estimate_total`：

```
available_input = context_window - reserve_output_tokens   // 输入可用预算（硬上限 & estimate/fits 共用）

fixed_overhead  = estimate_tokens(system_prompt)
                + estimate_tokens(serialize(tool_schemas))
                + protocol_wrapper_tokens                  // 常量，如 128

estimate_total  = 有 usage 基线 ? 基线 + 新增消息增量估算          // 基线已含 fixed_overhead
                                : fixed_overhead + 全量正文估算    // 回退：正文外再补固定开销
```

- `reserve_output_tokens`：新增预算配置字段，默认给一个保守值（如 1024）；CLI 可按模型调整。
- `protocol_wrapper_tokens`：常量，覆盖 provider 请求体骨架 + 消息 role/type 包装的固定 token 开销。
- 触发条件统一为 `estimate_total > available_input`；`estimate`/`fits`/`truncate` 三者共用同一 `estimate_total` 口径。
- **关键**：usage 基线（provider 的 `prompt_tokens`/`input_tokens`）本就是**总输入**测量，已含 system+tools+protocol。基线路径**不再二次扣减 `fixed_overhead`**；`fixed_overhead` 仅在「无 usage 回退」路径叠加，用于从正文估算重构总输入。

### 2. 消息 token 估算：实际 usage 优先

- `AssistantMessage.usage` 已携带 `Usage { input, output, .. }`（002/007 定稿）。维护「最近一次已知 input token 基线」：
  - 取 transcript 中**最后一条** `AssistantMessage.usage.input` 作为基线（provider 上报的**总输入 token**，已含 system+tools+protocol+历史正文）。
  - 对基线之后**新增**的消息（那条 assistant 之后的 User/Assistant/ToolResult），用启发式（`chars/4`）增量估算正文。
  - 当前估算 `estimate_total` = 基线 + 新增消息估算。
- 无任何 usage 时，回退：`estimate_total = fixed_overhead + 全量正文 chars/4`（正文估算之外再补固定开销）。
- **基线路径不再扣 `fixed_overhead`**（基线已含），避免与 §1 的 `available_input` 比较时二次扣减导致过早截断/压缩。
- `estimate_tokens` 启发式保持 `chars/4`（粗估，与 003/008 一致），不引入 tokenizer。

### 3. 配置字段

`CompactionPolicy`（008 定稿）新增两字段（additive）：

```rust
pub struct CompactionPolicy {
    pub budget_tokens: usize,        // 既有：压缩触发软阈值（比较对象 estimate_total；与 context_window 正交，041 语义不变）
    pub keep_recent: usize,          // 既有：保留最近 N turn（041 升级）
    pub reserve_output_tokens: usize,   // 新增：预留输出（从 context_window 扣，得 available_input）
    pub protocol_wrapper_tokens: usize, // 新增：协议包装固定开销（仅回退路径叠加进 estimate_total）
}
```

- `Default` 保守：`reserve_output_tokens` 与 `protocol_wrapper_tokens` 取合理默认，保证等价「几乎不压缩」。
- `budget_tokens` 语义：保持 041「压缩触发软阈值、与 `context_window` 正交」不变；比较对象由「正文估算」改为 `estimate_total`（预计总输入），触发更精确。**不再**套用「总窗口 = `budget_tokens + reserve_output_tokens + fixed_overhead`」的拆分式定义。

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
- [ ] 预算公式单测：`estimate_total`（回退）= `fixed_overhead + 正文估算`；`available_input = context_window - reserve_output`；`estimate_total > available_input` 触发压缩/截断
- [ ] usage 基线路径：构造含 `usage.input` 的 transcript → `estimate_total = 基线 + 新增消息估算`，且**不**再扣 `fixed_overhead`（断言无二次扣减）；无 usage → 回退 `fixed_overhead + 全量 chars/4`
- [ ] `estimate`/`fits`/`truncate` 口径一致：对同一 transcript，`fits(msgs)` 与 `truncate(msgs)` 结果不矛盾（构造「携带 `usage.input` 时 `fits` 为真则 `truncate` 不再截断」用例）
- [ ] `reserve_output_tokens`/`protocol_wrapper_tokens` 计入预算（边界断言：空/短/长文本）
- [ ] 产品代码无 `unwrap()`；更新所有 `CompactionPolicy` 构造点
- [ ] 单文件 ≤ 400 行，超则拆子模块并记录

## 修订记录

- v1.1（2026-09-22，Architect，依据 Review R1 设计疑问定稿）：① 修正预算口径为「预计总输入 token」——`available_input = context_window - reserve_output`，`estimate`/`fits`/`truncate` 统一复用 `estimate_total`；`fixed_overhead` 仅在无 usage 回退路径叠加，**usage 基线路径不再二次扣减**（基线已含 system+tools+protocol）。② `budget_tokens` 语义回退为 041 原义（压缩触发软阈值、与 `context_window` 正交），比较对象改为 `estimate_total`，放弃「总窗口 = budget_tokens + reserve + fixed_overhead」拆分式定义。
- v1.0（2026-09-13，Architect）：初稿。依据维护审查次要项 #1：预算改为 `context_window - reserve_output_tokens` 再扣固定开销（system/tools/protocol）；消息估算优先用最后一条 `AssistantMessage.usage.input` 作基线 + 新增消息增量估算，无 usage 回退 `chars/4`；`CompactionPolicy` 增两字段（additive）。
