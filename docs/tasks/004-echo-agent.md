# Task 004: 最小 Echo Agent（端到端）

## Background

001–003 已交付消息/事件、AgentHandle 生命周期、Runtime 主循环（fake provider 驱动）。本任务打通端到端：一个可运行的示例（bin）+ 集成测试，验证从 `prompt` 到事件流、到最终 transcript 的完整链路。同时落地内置工具的最小化实现（echo 工具即可，为后续 005+ 内置工具铺路）。

> ⚠️ 004 的旧实现（2026-08-22 提交，commit 567265b）早于 001 v1.1 / 003 定稿，Tool 签名、事件契约、bin 结构均已过期。**需按本规格（v1.2）在定稿架构上重做**，不能沿用旧代码。

## Goal

- 提供一个 `echo` 工具与示例 agent
- `src/bin/main.rs`：单次 prompt 的 CLI 示例（打印 transcript / 事件摘要）
- `tests/echo_agent.rs`：端到端集成测试，真实走 AgentHandle → Runtime loop → Tool
- 明确一期不接真实 HTTP：示例默认用 fake provider（后续接 adapters/）

## Design Notes

### Echo 工具（实现 Tool trait，签名沿用 003 定稿）

```rust
pub struct EchoTool;   // name="echo"，读 args.message，返回原文
```

- 实现 003 定稿的 `Tool` trait 完整签名：
  `execute(&self, tool_call_id: &str, args: serde_json::Value, signal: CancellationToken, on_update: Option<&dyn Fn(ToolResult)>) -> Result<ToolResult, ToolError>`
- `resource_scope: ReadOnly`（可并行，验证并发路径）；echo 无副作用，`on_update` 可忽略；参数校验失败直接返回 `ToolError`
- 参数宽松校验：`serde_json::from_value::<EchoArgs>()`，`EchoArgs { message: String }`
- 注册方式：`AgentConfig` 增加 `tools: Vec<Arc<dyn Tool>>` 字段（001/003 未显式定该字段，本任务补契约），经 `AgentHandle::spawn(config)` 传入，runtime 循环据此编排工具执行

### 端到端集成（tests/echo_agent.rs）

- 复用 `tests/common/mod.rs` 的 `FakeProvider`（003 已建，勿复制），配置**两轮回放**：
  1. 工具轮：`ToolCallStart { id, name: "echo", arguments: {"message":"hello"} } → ToolCallEnd → Done`，触发 EchoTool 执行
  2. 文本轮：`TextDelta("echo: hello") → Done`，产出终态 assistant 文本
- `AgentHandle::spawn(AgentConfig { tools: vec![Arc::new(EchoTool)] })`
- `prompt("hello")` → `wait_for_idle`（唯一同步点，禁止 `sleep`）
- 断言（snapshot + 事件交叉验证）：
  - snapshot 最终 `messages` 含 `User`、含 `ToolResult`（`tool_name=="echo"`）、含终态 `Assistant`（`stop_reason == Some(Completed)`）
  - 事件订阅收到**完整有序序列**：`AgentStart` 开头 → 两次 `TurnStart/TurnEnd`（两轮）→ `ToolExecutionStart/ToolExecutionEnd` 包裹工具执行 → 末尾 `AgentEnd`。不得只断言"收到过某事件"

### CLI 示例（src/bin/main.rs）

- 入口固定 `cargo run --bin guigu`（Cargo.toml 已显式 `[[bin]] name="guigu" path="src/bin/main.rs"`）
- **删除根 `src/main.rs`**：它与 `src/bin/main.rs` 同名 bin（都叫 `guigu`）冲突，只保留 `src/bin/main.rs`
- 从 stdin 读一行 → prompt → 打印最终 assistant 文本
- 默认用 fake provider（注释说明真实 provider 待 adapters 任务）

### lib.rs 聚合导出（本任务补齐）

- 作为 facade，供嵌入方 `guigu = { path = ... }` 使用
- `pub use core::{message::*, event::*, agent::*, tool::*, provider::*, runtime::*};`
- `runtime` 已拆为子模块目录（`runtime/mod.rs` + `turn.rs`/`tools.rs`/`step.rs`），需在 `runtime/mod.rs` 内 re-export 对外公开项，保证 `runtime::*` 可用
- `src/tools/` 经顶层 `lib.rs` 一并导出（`pub use tools::*`，EchoTool 需对外可见）

### 行为契约延续

- 端到端执行中，一个 run 一个 CancellationToken 贯穿（003 已实现，本任务仅验证传递）
- 对外只读 `AgentSnapshot`，测试断言基于 snapshot 而非内部状态

## Files

- src/tools/echo.rs（EchoTool 实现，落 `src/tools/` 而非 `core/tool.rs`；`src/tools/mod.rs` 声明 `pub mod echo`）
- src/bin/main.rs（CLI 示例）
- src/main.rs（**删除**，与 src/bin/main.rs 同名 bin 冲突）
- src/lib.rs（聚合导出，补 `tools::*` 与 runtime 子模块 re-export）
- src/core/runtime/mod.rs（若需补 re-export 公开项）
- tests/echo_agent.rs（端到端，复用 tests/common/mod.rs 的 FakeProvider）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets -D warnings passes（含 bin、集成测试）
- [ ] cargo test --all-targets passes
- [ ] cargo fmt --check passes
- [ ] `cargo run --bin guigu` 从 stdin 读一行，经 fake provider 两轮回放输出 assistant 回复
- [ ] tests/echo_agent.rs 端到端通过：snapshot 含 User / ToolResult(tool_name=="echo") / 终态 Assistant(stop_reason==Completed)；事件为完整有序序列（AgentStart 开头、两次 Turn、ToolExecutionStart/End 包裹、AgentEnd 末尾），非"收到过"式松散断言
- [ ] 复用 tests/common/mod.rs 的 FakeProvider，无重复定义
- [ ] 删除 src/main.rs，仅保留 src/bin/main.rs（同名 bin 冲突消除）
- [ ] 产品代码无 `unwrap()`；测试内用 `expect("前置条件")`；bin 用 `?` + `main -> Result`
- [ ] 单文件超 400 行拆子模块

## 修订记录

- v1.2（2026-08-26，Architect 二次重核验）：v1.1 仅更新修订记录未落实正文，正文仍残留旧表述（单轮回放、"收到过"断言、"由实现者定"）。本次将修订记录所述变更**写入正文**，消除歧义。
  - 端到端集成改为两轮回放 + 完整事件序列断言 + stop_reason==Completed
  - CLI 明确 `cargo run --bin guigu` 并删除 `src/main.rs`
  - lib.rs 导出明确 runtime 子模块 re-export 与 tools 导出
  - Files 明确 EchoTool 落 `src/tools/echo.rs`
  - 验收标准统一 `--all-targets`、复用 FakeProvider、删除 src/main.rs
- v1.1（2026-08-26，Architect 重核验）：对齐 001 v1.1 / 003 定稿后的架构。
  - Echo 工具补 003 定稿 `Tool` 完整签名（signal / on_update）
  - `AgentConfig.tools` 字段契约（001/003 未显式定义，本任务补）
  - FakeProvider 改为两轮回放（工具轮 + 文本轮），修正单轮 `Done` 后无终止的歧义
  - 事件断言按 001 定稿严格化（完整序列，非"收到过"）
  - lib.rs 导出适配 runtime 子模块拆分
  - 明确 `src/main.rs` 与 `src/bin/main.rs` 同名 bin 冲突的处理
