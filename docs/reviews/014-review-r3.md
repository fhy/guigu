# Task 014 Review - Round 3

## 基本信息
- 审查时间: 2026-09-03
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/014-acp-adapter.md
- 审查范围: 当前工作树（Task 014 round 2 修复）

## 门禁结果
- cargo check: ✓
- cargo clippy --all-targets -D warnings: ✗（测试目标编译失败，并报告 3 个未使用导入及 `StdioConnection::new` 缺少 `Default`）
- cargo test --all-targets: ✗（测试目标编译失败）
- cargo fmt --check: ✗（`src/acp/mod.rs`、`src/acp/stdio_client.rs`、`src/acp/tests_transport.rs`、`src/acp/transport.rs` 未格式化）

## 代码审查
### 问题
1. [Critical] `src/acp/tests_transport.rs:358-372` — `FailingWriter` 的 `AsyncWrite` 实现签名错误。
   - 影响: 当前使用的 tokio 版本要求 `poll_write`、`poll_flush`、`poll_shutdown` 的 receiver 为 `Pin<&mut Self>`，导致所有测试目标无法编译，直接阻断 `cargo test --all-targets` 和 clippy 门禁。
   - 建议: 将三个方法改为 `self: Pin<&mut Self>`，并补充 `use std::pin::Pin;`；该类型无自引用，可直接忽略 pin 或通过 `get_mut()` 使用。

2. [Critical] `src/acp/tests_fs.rs:26,59,89,131,155` — 调用 `AcpFsTool::execute` 时未将 `Tool` trait 引入作用域。
   - 影响: trait 提供的方法无法通过 `tool.execute(...)` 解析，导致测试目标再次编译失败。
   - 建议: 在文件顶部引入 `use crate::core::tool::{Tool, ToolError};`。

3. [Warning] `src/acp/testutil.rs:13-14`、`src/acp/tests.rs:11,15,17` — 存在未使用导入。
   - 影响: 在 `-D warnings` 下会将 warning 升级为错误；即使修复前两项，clippy 仍会失败。
   - 建议: 删除 `testutil.rs` 中未使用的 `json`、`CancellationToken`，删除 `tests.rs` 中未使用的 `Value`、`AcpAgent`、`AcpClient`、`AssistantContent`。

4. [Warning] `src/acp/stdio_client.rs:138-146` — `StdioConnection::new` 触发 clippy `new_without_default`。
   - 影响: `cargo clippy --all-targets -- -D warnings` 失败。
   - 建议: 为 `StdioConnection` 实现 `Default`，转发至 `Self::new()`，或将构造接口调整为符合项目惯例的默认构造方式。

5. [Warning] 格式检查失败涉及 `src/acp/mod.rs:28-35`、`src/acp/stdio_client.rs:97-103`、`src/acp/tests_transport.rs:15,274,382-386`、`src/acp/transport.rs:32-36`。
   - 影响: 不符合任务验收标准 `cargo fmt --check`。
   - 建议: 修复编译错误后执行 `/home/fhy/.cargo/bin/cargo fmt`，再运行 `cargo fmt --check`。

## 建议
1. 当前 `serve_connection_with` 的 writer 错误回归测试在测试任务启动后立即进入连接循环，建议修复编译后实际确认 pending request 已被 `cancel_all` 唤醒，避免仅验证连接返回而未验证错误内容。
2. 修复后必须按顺序重跑 `cargo check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all-targets`、`cargo fmt --check`。

## 结论
- [ ] 通过
- [x] 打回

## 下一步
- @guigu-worker 请修复问题 1-5，提交前完成四道门禁后申请 Task 014 复审。
