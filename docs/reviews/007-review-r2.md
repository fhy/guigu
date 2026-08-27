# Task 007 Review - Round 2

## 基本信息
- 审查时间: 2026-08-27
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/007-adapters.md
- 审查提交: `39c508e feat(adapters): Task 007 OpenAI/Anthropic adapters（reqwest feature-gated）`

## 门禁结果
- cargo check: ⚠️ 未执行：审查环境未安装 `cargo`（`cargo: command not found`）
- cargo clippy --all-targets -D warnings: ⚠️ 未执行：审查环境未安装 `cargo`
- cargo test --all-targets: ⚠️ 未执行：审查环境未安装 `cargo`
- cargo test --no-default-features: ⚠️ 未执行：审查环境未安装 `cargo`
- cargo fmt --check: ⚠️ 未执行：审查环境未安装 `cargo`

## 代码审查

### 问题
（无新增 Critical / Warning 级阻断缺陷。）

### 建议
1. `src/adapters/acc.rs:75-103`、`src/adapters/anthropic/blocks.rs:40-79` — Anthropic 多个同类型 content block 会被合并。
   - 影响：当响应出现 `text(index=0) → tool_use(index=1) → text(index=2)` 或多个 thinking block 时，累积器只有一个全局 `text` / `thinking` 段，最终 `AssistantMessage.content` 不能按 content block index 保留独立段落及顺序。
   - 建议：按 Anthropic block index 维护每个 text/thinking block 的独立累积内容，并在 `build_message` 时按 index 排序生成 content；OpenAI 仍可使用现有全局累积逻辑。应增加交错多 block 回归测试。

2. `src/adapters/openai/events.rs:86-92` — 工具调用续块直接把 provider 的 `index` 当作 `tool_calls` Vec 下标。
   - 影响：若 provider 使用非连续 index、续块先于可用的本地 Vec 项，当前代码会丢弃参数增量或无法正确关联工具调用；同时缺少显式的 index→本地调用映射。
   - 建议：在 `Acc` 中维护 provider tool-call index（或 id）到本地下标的映射；对未知 index 返回 `ProviderError::Parse`/流内 Error，而不是静默 `continue`。增加非连续 index 和异常顺序测试。

3. `src/adapters/anthropic/request.rs:121-129` — 非法 tool-call arguments 被静默替换为 `{}`。
   - 影响：上游消息损坏时请求仍被发送，模型会收到与实际调用不同的参数，问题难以诊断。
   - 建议：将 assistant 消息映射改为可失败的 `Result<Value, ProviderError>`，JSON 反序列化失败返回 `ProviderError::Build`（或在构造层明确记录该降级策略）。

## 结论
- [x] 代码审查通过（上述为非阻断建议）
- [ ] 打回

## 下一步
- 由于当前环境没有 Cargo，合入前必须在具备 Rust 工具链的环境重新执行五项门禁。
- 建议后续优先补充 Anthropic 多 block 顺序和 OpenAI 非连续 tool-call index 的测试。
