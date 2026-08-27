# Task 007 Review - Round 3

## 基本信息
- 审查时间: 2026-08-27
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/007-adapters.md
- 审查提交: `39c508e feat(adapters): Task 007 OpenAI/Anthropic adapters（reqwest feature-gated）`

## 门禁结果
- cargo check: ⚠️ 未执行：当前审查环境未安装 `cargo`（`cargo: command not found`）
- cargo clippy --all-targets -- -D warnings: ⚠️ 未执行：当前审查环境未安装 `cargo`
- cargo test --all-targets: ⚠️ 未执行：当前审查环境未安装 `cargo`
- cargo test --no-default-features: ⚠️ 未执行：当前审查环境未安装 `cargo`
- cargo fmt --check: ⚠️ 未执行：当前审查环境未安装 `cargo`

## 代码审查

### 问题
1. **[Critical]** `src/adapters/acc.rs:75-103, 139-166`、`src/adapters/anthropic/blocks.rs:40-79` — Anthropic 的多个同类型 content block 被合并。
   - 影响：`text(index=0) → tool_use(index=1) → text(index=2)` 会把两个文本块都追加到全局 `Acc::text`，最终只生成一个 Text；多个 thinking block 也相同。这样不符合规格要求的“按 content_block index 升序保留内容段及顺序”，会改变模型消息语义。
   - 建议：按 Anthropic block index 保存每个 text/thinking block 的独立累积内容（工具块也保留 index 映射），`build_message` 时按 index 排序生成内容；不要用全局 `text`/`thinking` 段代替多个 block。补充交错 text/tool/text 及多个 thinking block 的回归测试。

2. **[Critical]** `src/adapters/openai/events.rs:64-100` — 工具调用续块把 provider 的 `index` 直接当作本地 `tool_calls` Vec 下标。
   - 影响：OpenAI 的首块可能使用非连续 index，或多个调用的 provider index 与本地 start 顺序不一致。此时参数增量会被静默丢弃（`get_mut` 返回 None），甚至可能追加到错误的工具调用，导致最终 tool arguments 损坏。
   - 建议：在 `Acc` 或 OpenAI 专用累积状态中维护 provider index → 本地工具调用下标的显式映射，并在未知 index、续块先于 start 时返回 `ProviderError::Parse`（由流层转成 `Error`），禁止 `continue`/静默忽略。补充非连续 index、多工具交错及异常顺序测试。

3. **[Warning]** `src/adapters/anthropic/request.rs:111-133` — 非法 assistant tool-call arguments 被静默替换为 `{}`。
   - 影响：历史消息或上游适配器产生损坏 JSON 时，请求仍会发送，但 Anthropic 收到的参数与原始调用不一致，错误被隐藏且难以诊断。
   - 建议：将 assistant 消息映射改为可失败的 `Result<Value, ProviderError>`，反序列化失败返回 `ProviderError::Build`；同步修改 `build_request` 和测试，验证非法参数不会被降级。

### 建议
1. `src/adapters/anthropic/blocks.rs:13-19, 55-61` — 对缺少/非法 `index` 或 block 类型字段的事件不要默认使用 `0`/空字符串；建议返回可诊断的 Parse 错误，避免异常响应污染第 0 个 block。
2. `tests/adapters.rs:44-242` — 端到端测试当前只覆盖单 text block 和 index=0 的单工具调用，未覆盖上述验收标准中的交错 block、非连续工具 index 及请求 body/header 断言；应增加真实请求格式断言和负向流测试。

## 结论
- [ ] 通过
- [x] 打回

## 下一步
- @guigu-worker 请修复以上 3 个问题，并补充对应回归测试。
- 修复后须在具备 Rust 工具链的环境重新执行：`cargo check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all-targets`、`cargo test --no-default-features`、`cargo fmt --check`。
