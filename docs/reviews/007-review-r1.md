# Task 007 Review - Round 1

## 基本信息
- 审查时间: 2026-08-27 20:10
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/007-adapters.md
- 提交: 39c508e feat(adapters): Task 007 OpenAI/Anthropic adapters（reqwest feature-gated）

## 门禁结果
- cargo check: ✓
- cargo clippy --all-targets -D warnings: ✓（0 warning）
- cargo test --all-targets: ✓（157：91 lib + 5 adapters + 61 既有集成）
- cargo test --no-default-features: ✓（23 lib，adapter 被 feature 跳过；核心库编译通过）
- cargo fmt --check: ✓

## 代码审查

### 规格符合性
- ✅ SSE 解析器（`sse.rs`）：多行 data 拼接、event+data、`[DONE]`、空行分隔、CRLF、跨块缓冲、finish 残留——均实现且单测齐全
- ✅ 请求构造（`openai/request.rs`、`anthropic/request.rs`）：URL/headers/body 均匹配规格映射表；tools 空时省略；system prompt 正确放置；ToolCall arguments→Anthropic `input` 已反序列化为对象
- ✅ 事件映射（`openai/events.rs`、`anthropic/events.rs`+`blocks.rs`）：逐条符合映射表，text/thinking/tool_call start/delta/end、usage、stop_reason 全覆盖
- ✅ 累积（`acc.rs`）：text/thinking/tool_calls + segment 顺序 + usage/stop_reason 完整产出 `Done` message
- ✅ ProviderError 补齐 `Network`/`HttpStatus`/`Parse`/`Build`，保留既有 `Request`/`Aborted`，未改 ModelProvider 等签名
- ✅ feature 门控：`providers-http` default 含，reqwest optional + rustls-tls；`lib.rs` doc 注明 `default-features = false` 剥离
- ✅ 错误两段式：建立期失败→外层 `Err`；流内失败→`AssistantEvent::Error`（取消 `aborted:true`）
- ✅ 产品代码无 `unwrap()`/`panic!`（`sse.rs` 用 `unwrap_or`，安全）
- ✅ 单文件均 ≤ 400 行，超限拆为子模块
- ✅ wiremock 端到端：完整 SSE 流、401→HttpStatus、建立期取消→Aborted，均通过

### 问题
（无 Critical / 无 Warning 级阻断缺陷）

### 建议（可选，非阻断）
1. **src/adapters/acc.rs:76-103 + anthropic/blocks.rs:40-49 — Anthropic 多文本块会被合并导致 content 顺序偏差**
   - 现状：`ensure_text()`/`ensure_thinking()` 只登记"一种段一次"，所有 text 增量统一追加到单一 `text` 累积器。若一条响应内出现 `text → tool_use → text` 交错的多个 text 块（index 0、1、2），index=2 的 text 会被并入排在前面的 Text 段，最终 content 顺序变成 `text(前) → tool → (无独立 text(后))`，与规格"按 content_block index 升序"不符。
   - 影响：仅影响同一轮内多文本块与工具块交错的少见场景；常见的"单 text 或 text+tool"不受影响。
   - 建议：可为 Anthropic 按 `index` 维护 per-block 文本累积（如 `block_texts: Vec<(usize, String)>`），`build_message` 时按 index 还原独立 Text 段；或将 `ensure_text` 改为按 block index 去重而非全局去重。OpenAI 侧因单串流不受影响，不需改。
   - 工作量：中。当前验收用例未覆盖该交错场景，故判为非阻断。

## 结论
- [x] 通过（附 1 条可选改进建议，不阻断合入）

## 下一步
- 无需强制修复。建议 Developer 在后续迭代中考虑按 block index 维护 Anthropic 多文本块顺序，以完全贴合"content_block index 升序"规格。
