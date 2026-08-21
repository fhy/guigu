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

---

# 复审 #1（2026-08-21）

- 结论: **REJECT（再次打回）**
- 审查对象: 工作区未提交改动（Cargo.toml、src/lib.rs、src/core/、src/main.rs、tests/message.rs）

## 门禁结果

| Gate | 结果 |
|------|------|
| cargo check | ✓ |
| cargo clippy -D warnings | ✗ 17 个错误 |
| cargo test | ✓ 7 passed / 0 failed |
| cargo fmt --check | ✓ |

## 上次问题修复情况

| 上次问题 | 状态 |
|----------|------|
| P0-1/2 依赖版本 | ✓ 已修（async-trait 0.1、tokio-test 0.4） |
| P0-3 edition 2024 | ✓ 已恢复 |
| P0-4 event.rs 引用路径 | ✓ 已修（crate::core::message::...） |
| P0-5 unused HashMap | ✓ 已删 |
| P0-6 缺 lib.rs | ✓ 已加 src/lib.rs，测试走 guigu::core |
| P0-7 serde rc feature | ⚠ feature 已加但代码未用 Arc（见新问题 5） |
| P0-8 serde tag 冲突 | ✓ 已按建议改 struct variant |
| **P0-9 StopReason 兜底** | ✗ **仍未实现** |
| **P1-10 AgentEvent 全变体** | ✗ **仍只测 AgentStart** |
| P1-11 StopReason 用例 | ✗ 仅 Other roundtrip，无兜底用例 |
| P1-12 内容段全枚举 | ✓ 真实通过 |
| P2-13 文档注释 | ✗ 仍全部缺失 |
| P2-14 main.rs 换行 | ✓ 已修 |
| P2-16 未提交 | ✗ 改动仍在工作区 |

## 新问题清单

### P0 — 门禁硬伤

1. **src/main.rs:1 — `mod core;` 触发 binary crate dead_code ×16**。main.rs 声明模块但从未使用，clippy -D warnings 下所有 core 类型报 "never used" → 删除 main.rs 的 `mod core;`（lib.rs 已导出，binary 需要时经 `guigu::core` 引用）。
2. **tests/message.rs:7 — `use serde_json;` 冗余 import**，clippy 报 redundant → 删除。

### P0 — AC 明确要求未满足（上次遗留）

3. **StopReason 未知值兜底未实现**（AC 第 5 条 + 上次问题 9）。实测 `{"type":"some_brand_new_reason"}` 反序列化报 `unknown variant` 错误而非落入 `Other` → 需自定义 Deserialize 或 untagged fallback，并补兜底测试用例。
4. **AgentEvent 全变体 roundtrip 未覆盖**（AC 第 5 条 + 上次问题 10）。9 个变体仍只测 AgentStart（tests/message.rs:104）→ 补齐其余 8 个变体的构造 + roundtrip 断言。

### P1 — 规格偏差未裁定

5. **AgentEvent 未使用 Arc**。规格 Design Notes 明确 `Vec<Arc<Message>>`、`Arc<AssistantMessage>`、`Arc<Message>`；实现为裸类型，serde `rc` feature 加了没用。事件流场景 Arc 可避免大消息 clone → 按规格补 Arc，或在回复中记录偏差请 PM/Architect 裁定。

### P2 — 建议性改进（不阻塞）

6. **ToolResult 双重定义**：src/core/tool.rs:6 与 src/core/event.rs:51 各有一份完全相同的 ToolResult，event.rs 内部用自己的、测试用 tool.rs 的 → 删一份统一引用。
7. 公开 API 全部缺 `///` 文档注释（conventions 要求）。
8. 依赖超范围：tokio/tokio-util/tracing/futures/async-trait 本任务均未使用，违反 "Minimal dependencies"，建议用到再加（thiserror 同理）。
9. ThinkingLevel 缺 serde tag 策略，当前序列化为 `"Medium"` 字符串形式，建议补 `rename_all = "snake_case"` 保持一致。
10. 测试中 `unwrap()` 严格按 AC 措辞应换 `expect`/assert（生产代码无 unwrap ✓）。

## 已验证的事实

- clippy 完整错误：16 × dead_code（binary crate）+ 1 × redundant import。
- StopReason 兜底探针测试失败：`unknown variant \`some_brand_new_reason\`, expected one of ...`。
- 合理偏差（已认可）：内容段 `Text(String)` → `Text { text }` struct variant（上次 review 裁定）；AssistantEvent 占位放 event.rs（规格允许，有注释）；ToolResult 放独立 tool.rs（规格允许，但见问题 6 重复定义）。

## 复审指引

四项门禁全绿 + 问题 3/4 测试真实覆盖 + 问题 5 有裁定结论 + 改动提交推送后，回复 `[Fix] Task 002` 申请复审。
