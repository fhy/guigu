# Task 023 Review - Round 1

## 基本信息
- 审查时间: 2026-09-09
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/023-tui.md
- 审查提交: aeabe14

## 门禁结果
- cargo check: ✓
- cargo clippy --all-targets -- -D warnings: ✓
- cargo test --all-targets: ✓（default 测试通过）
- cargo test --features tui --all-targets: ✓（320 库测试、48 binary 测试及集成测试通过）
- cargo test --no-default-features: ✓（225 库测试及集成测试通过）
- cargo fmt --check: ✓

## 代码审查
### 问题
1. [Critical] `src/bin/guigu/tui/mod.rs:60-75` — 终端初始化失败路径没有调用 `server.shutdown()`。
   - `enable_raw_mode`、进入 alternate screen 或 `Terminal::new` 任一步失败时，函数直接 `?` 返回；但 `run` 的调用方已经完成了 `setup_session`/`spawn_lane_from_factory`，server 可能已有运行时任务。规格要求退出前执行 `server.shutdown`，当前只覆盖了进入事件循环后的正常/异常退出，无法覆盖初始化失败路径。
   - 影响：无 TTY、终端初始化失败时可能遗留 agent/runtime task；长期运行或嵌入调用场景会泄漏资源，且生命周期契约不完整。
   - 建议：将终端 setup 封装为带清理 guard 的阶段，或在每个 setup 失败分支显式执行 `server.shutdown().await` 后再返回原始 `CliError`；同时保证已启用 raw mode/已进入 alternate screen 的阶段按已完成步骤恢复。建议为 setup 失败路径增加可注入/可测试的清理逻辑。

2. [Major] `src/bin/guigu/tui/mod.rs:155-157` — `server.prompt(...).await` 在键盘事件分支中串行等待，违反“不阻塞 UI”设计。
   - 规格 §5 明确要求提交 prompt 不阻塞 UI、事件回环由订阅驱动。当前 `tokio::select!` 分支进入后直接 await `server.prompt`；当 server/provider 入队或命令响应异常变慢时，无法处理键盘、agent 事件和 100ms tick，导致界面冻结，流式输出也不会及时渲染。
   - 建议：提交动作通过独立 tokio task/已有非阻塞 command enqueue 路径触发；错误通过专用 channel 回送 UI 状态。若 API 语义必须 await，应把 future 放入 select 管理且确保键盘/事件分支仍可继续轮询，并补充测试验证 prompt 未完成期间仍能消费事件。

3. [Major] `src/bin/guigu/tui/mod.rs:181` — `terminal.draw(...)` 错误被静默丢弃。
   - 终端断开、写失败等渲染错误会被 `.ok()` 丢掉，事件循环继续运行；用户可能看不到界面，直到另一个无关退出条件发生。此处也无法把 I/O 错误传给 `run` 做统一恢复和 shutdown。
   - 建议：将 draw 错误转换为 `CliError::Tui` 并 `break/return`，由外层统一恢复终端并 shutdown；至少记录错误并停止循环。不要静默忽略产品路径的终端 I/O 错误。

### 建议
1. `src/bin/guigu/tui/mod.rs:84-96` — reader 对 `poll`/`read` 错误直接退出且没有通知主循环，主循环只能看到 channel close；建议发送明确错误或记录原因，便于区分终端断开与正常退出。
2. `src/bin/guigu/tui/state.rs:222-224` — `ToolCallDelta`/`ToolCallEnd` 被忽略。当前工具执行事件通常可提供完整 args，但若 provider 只分片发送工具参数，UI 会显示空/不完整参数。若这是架构边界，应在任务文档明确；否则应累积 delta 并在 end 时更新卡片。
3. `src/bin/guigu/tui/state.rs:315-317` — 测试经 `#[path]` 拆分后可读性尚可，但测试辅助函数中仍使用 `panic!`，不影响产品路径；可在后续统一改为断言式失败信息。

## 结论
- [ ] 通过
- [x] 打回

## 下一步
- @guigu-worker 请修复问题 1-3，重点保证所有终端 setup/渲染失败路径都执行终端恢复和 `server.shutdown`，并消除 prompt await 对 UI 事件循环的阻塞。
- 修复后重新运行 `cargo check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all-targets`、`cargo test --features tui --all-targets`、`cargo test --no-default-features`、`cargo fmt --check`。
