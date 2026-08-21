# Review: Task 002 — Message/Event 数据结构

- 日期: 2026-08-21
- 结论: **REJECT（打回）**
- 审查方式: 通读规格 + 全量 DoD 门禁。注：原始代码无法通过 `cargo check`，reviewer 做了临时验证性修复（仅本地，已还原，未提交）以探明后续问题，下述测试结论基于最小可编译状态。

## 门禁结果（worker 提交时的原始状态）

| Gate | 结果 |
|------|------|
| cargo check | ✗ 依赖解析失败（async-trait 版本不存在） |
| cargo clippy -D warnings | ✗ 未使用 import 等（check 通过后仍会挂） |
| cargo test | ✗ 最小修复后 4/5 失败 |
| cargo fmt --check | ✗ event.rs 格式不符 |

## 问题清单

### P0 — 编译 / 门禁硬伤

1. **Cargo.toml:18** — `async-trait = "1.0"` 版本不存在（crates.io 上为 0.1.x），`cargo check` 直接失败 → 改为 `"0.1"`。
2. **Cargo.toml:20** — `tokio-test = "1.0"` 版本错误（实际为 0.4.x），且未被任何测试使用 → 改 `"0.4"` 或删除。
3. **Cargo.toml:3** — edition 从 2024 被**降级**为 2021，违反 conventions（Rust edition 2024）→ 恢复 `edition = "2024"`。
4. **src/core/event.rs:4** — `use crate::core::{Message, ...}` 引用不存在的 re-export，E0432 编译失败 → 在 `src/core/mod.rs` 增加 `pub use message::{...}` / `pub use event::{...}`，或改为 `use crate::core::message::{...}`。
5. **src/core/message.rs:2** — 未使用的 `use std::collections::HashMap;` → 删除（clippy -D warnings 必挂）。
6. **缺少 src/lib.rs** — 集成测试 `tests/message.rs:2` 用 `crate::core::...` 访问 binary crate 内部模块，集成测试是独立 crate，必然失败 → 新增 `src/lib.rs`（`pub mod core;`），测试改为 `use guigu::core::...`。
7. **Cargo.toml:12** — `AgentEvent` 使用 `Arc<Message>` / `Arc<AssistantMessage>`，serde 对 `Arc<T>` 的 impl 需要 `rc` feature，否则 20 个 E0277 → `serde = { version = "1.0", features = ["derive", "rc"] }`。

### P0 — 规格符合性（核心设计问题）

8. **serde tag 策略与 newtype variant 冲突（4/5 测试失败的根因）**
   `#[serde(tag = "type")]`（internally tagged）不支持内容为非 map 的 newtype variant。实测报错：
   `cannot serialize tagged newtype variant UserContent::Text containing a string`
   受影响：`UserContent::Text(String)`、`AssistantContent::Text/Thinking(String)`、`StopReason::Other(String)`。
   → 建议改为 struct variant，保持 Pi JSON 形状 `{"type":"text","text":"..."}`：
   ```rust
   pub enum UserContent {
       Text { text: String },
       Image(ImageContent),
   }
   ```
   注意：规格 Design Notes 中 `Text(String)` + `tag = "type"` 的组合在 serde 下不可实现，属规格自身缺陷。JSON 形状是 001/003/004 的跨任务契约，**请 PM 裁定**：由 worker 按 struct variant 实现并在回复中记录偏差，或先回 Architect 修订规格。

9. **StopReason 未知值兜底未实现**（规格明确要求"任意未知字符串走 Other"）
   当前反序列化未知 `type` 直接报错，不会落入 `Other`。internally tagged 下需自定义 `Deserialize`（或 untagged fallback）实现兜底。

### P1 — 测试覆盖不足（AC 明确要求）

10. **AgentEvent 全变体 roundtrip 未覆盖** — 9 个变体只测了 `AgentStart`（tests/message.rs:89）。至少补齐各变体构造 + roundtrip 断言。
11. **StopReason 仅测 `Other` 且该测试本身失败** — 补齐 `Completed` 等具名变体 + 未知字符串兜底用例。
12. 内容段全枚举用例因问题 8 全部失败，修复后需确认真实通过。

### P2 — 建议性改进（不阻塞）

13. 所有 pub 类型缺 `///` 文档注释（conventions 要求公开 API 有文档）。
14. src/main.rs 文件末尾缺换行符（fmt --check 会挂）。
15. `thiserror` 已声明未使用——数据层暂无错误类型可接受，后续任务用到再留；`tokio-test` 建议随问题 2 一并处理。
16. **流程**：本次改动（src/core/、tests/、Cargo.toml、main.rs）均未 commit/push。conventions 要求完成即提交，修复后请一并提交。

## 已验证的事实

- `StopReason::Other("custom_reason")` 序列化 panic（tests/message.rs:81）。
- `UserContent::Text` 序列化 panic（tests/message.rs:16）。
- 修复编译后 `cargo test`：1 passed, 4 failed（唯一通过的是 `AgentEvent::AgentStart` roundtrip）。

## 复审指引

修复后请确保：四项门禁全绿 + 问题 8/9/10/11 的测试真实覆盖 + 回复中记录对规格的偏差（如有）。
