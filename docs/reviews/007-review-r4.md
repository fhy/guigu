# Task 007 Review - Round 4

## 基本信息
- 审查时间: 2026-08-27
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/007-adapters.md
- 审查提交: `11e1afa fix(adapters): Task 007 r3 打回修复（block 独立累积 / tool index 显式映射 / 非法参数 Build 错误）`

## 门禁结果
- cargo check: ✓
- cargo clippy --all-targets -- -D warnings: ✓
- cargo test --all-targets: ✓（176 passed，0 failed）
- cargo test --no-default-features: ✓（核心测试 75 passed，0 failed；adapters 已正确门控）
- cargo fmt --check: ✓

## 代码审查
### 问题
1. **[Critical]** `src/adapters/anthropic/blocks.rs:29-60` — 未拒绝重复的 Anthropic `content_block_start.index`。
   - 影响：同一个 `index` 再次收到 `content_block_start` 时，`start_text_block`/`start_thinking_block`/`start_tool_call` 已经先向 `segments` 和对应数组追加了新段，随后 `note_block` 直接覆盖旧映射（`src/adapters/acc.rs:192-198`），不会返回 `Parse`。这样会留下一个无法再由该 index 定位的幽灵段，并可能在 `Done` 中输出重复/错误内容；也与本次修复说明中“同一 index 重复 start 返回 Parse”不一致。
   - 建议：在 `handle_block_start` 开始处先检查 `acc.block_kind(index).is_some()`，若已存在立即返回 `ProviderError::Parse`，再创建和登记新 block；补充重复 text、tool_use 或 thinking index 的回归测试，并断言累积状态未被污染。

## 建议
1. `src/adapters/anthropic/blocks.rs:153-174` — 可考虑为重复 `content_block_stop` 增加状态校验，避免同一 tool call 重复发送 `ToolCallEnd`；若协议允许重复 stop，应在规格中明确并测试。
2. `src/adapters/openai/events.rs:83-92` — 重复 index 的错误发生在 `start_tool_call` 之后，会短暂留下未映射的累积项。当前流会终止，因此不会影响已返回的 `Done`，但可先校验映射占用情况或提供原子化的 start+map 操作，保持错误路径状态一致。

## 结论
- [ ] 通过
- [x] 打回

## 下一步
- @guigu-worker 请修复上述 Critical 问题，补充重复 Anthropic block index 测试，并重新执行五项门禁。
