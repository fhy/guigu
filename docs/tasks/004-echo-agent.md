# Task 004: 最小 Echo Agent（端到端）

## Background

001–003 已交付消息/事件、AgentHandle 生命周期、Runtime 主循环（fake provider 驱动）。本任务打通端到端：一个可运行的示例（bin）+ 集成测试，验证从 `prompt` 到事件流、到最终 transcript 的完整链路。同时落地内置工具的最小化实现（echo 工具即可，为后续 005+ 内置工具铺路）。

## Goal

- 提供一个 `echo` 工具与示例 agent
- `src/bin/main.rs`：单次 prompt 的 CLI 示例（打印 transcript / 事件摘要）
- `tests/echo_agent.rs`：端到端集成测试，真实走 AgentHandle → Runtime loop → Tool
- 明确一期不接真实 HTTP：示例默认用 fake provider（后续接 adapters/）

## Design Notes

### Echo 工具（实现 Tool trait）

```rust
pub struct EchoTool;   // name="echo"，读 args.message，返回原文
```

- `resource_scope: ReadOnly`（可并行，验证并发路径）
- 参数宽松校验：`serde_json::from_value::<EchoArgs>()`
- 注册方式：`Vec<Arc<dyn Tool>>` 传入 agent 配置（`AgentConfig { tools: Vec<Arc<dyn Tool>> }`）

### 端到端集成（tests/echo_agent.rs）

1. 构造 `FakeProvider`：回放 `ToolCallStart(echo) → ToolCallEnd → Done`
2. `AgentHandle::spawn` + `AgentConfig { tools: [echo] }`
3. `prompt("hello")` → `wait_for_idle`
4. 断言：snapshot 最终 messages 含 user 消息、含 ToolResultMessage（tool_name="echo"）、含终态 AssistantMessage；事件订阅收到 `ToolExecutionStart/End` 与 `AgentEnd`

### CLI 示例（src/bin/main.rs）

- `cargo run --example` 或 `cargo run --bin guigu`（实现者按 Cargo.toml 现有设置定）
- 从 stdin 读一行 → prompt → 打印最终 assistant 文本
- 默认用 fake provider（README/注释说明真实 provider 待 adapters 任务）

### lib.rs 聚合导出（本任务补齐）

- `pub use core::{message::*, event::*, agent::*, tool::*, provider::*, runtime::*};`
- 作为 facade，供嵌入方 `guigu = { path = ... }` 使用

### 行为契约延续

- 端到端执行中，一个 run 一个 CancellationToken 贯穿（003 已实现，本任务仅验证传递）
- 对外只读 `AgentSnapshot`，测试断言基于 snapshot 而非内部状态

## Files

- src/bin/main.rs
- src/lib.rs（聚合导出，可能 003 已建，本任务补齐）
- src/core/tool.rs（已有 trait，本任务加 echo 工具实现；若放 src/tools/echo.rs 更好，由实现者定并记录）
- tests/echo_agent.rs

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy -D warnings passes
- [ ] cargo test passes
- [ ] cargo fmt --check passes
- [ ] `cargo run`（bin 示例）能从 stdin 输入并输出 assistant 回复（fake provider 路径）
- [ ] tests/echo_agent.rs 端到端通过：snapshot 含 User/ToolResult/Assistant，事件含 ToolExecution 与 AgentEnd
- [ ] 无 `unwrap()`（bin 示例可用 `?` + main 返回 `Result`，或打印错误）
- [ ] 单文件超 400 行拆子模块
