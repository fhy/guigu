# Task 023 Review - Round 2

## 基本信息
- 审查时间: 2026-09-10
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/023-tui.md
- 审查提交: ad9bbd8

## 门禁结果
- cargo check: ✓
- cargo clippy --all-targets -- -D warnings: ✓
- cargo test --all-targets: ✓（320 库测试、18 binary 测试及集成测试通过）
- cargo test --features tui --all-targets: ✓（320 库测试、52 binary 测试及集成测试通过）
- cargo test --no-default-features: ✓（225 库测试及集成测试通过）
- cargo fmt --check: ✓

## 代码审查
### 问题
1. [Critical] `src/bin/guigu/tui/mod.rs:187-191` — alternate screen 与隐藏光标在同一次 `execute!` 中执行，但失败分支只关闭 raw mode。
   - `execute!(stdout, EnterAlternateScreen, Hide)` 会顺序执行命令；若进入 alternate screen 已成功、随后 `Hide` 写入失败，当前分支不会执行 `LeaveAlternateScreen`/`Show`。这仍未满足规格“终端状态异常时也恢复”，也未完整修复 r1 问题 1 所要求的“恢复已完成步骤”。
   - 影响：部分写失败时用户终端可能遗留在 alternate screen 或光标隐藏状态。
   - 建议：拆分 `EnterAlternateScreen` 与 `Hide` 并逐阶段清理，或该失败分支统一调用包含 `LeaveAlternateScreen`、`Show`、`disable_raw_mode` 的恢复函数；最好用显式 setup guard 记录阶段。

2. [Major] `src/bin/guigu/tui/mod.rs:164-171` — 退出时先等待 command task，再调用 `server.shutdown()`，可能永久阻塞退出。
   - `spawn_command_task` 可阻塞在 `server.prompt().await`；底层 agent command channel 是容量 100 的 bounded channel。用户连续提交使其填满、同时 provider/run 长时间不返回时，`drop(cmd_tx)` 只能阻止新命令，无法取消正在等待入队的 prompt，`cmd_task.await` 因此阻止后续 shutdown。
   - 影响：Esc、第二次 Ctrl-C、draw/read 错误路径均可能无法恢复到完成 shutdown，TUI 进程挂住。
   - 建议：退出时主动取消/abort command task 后再 shutdown，不要无期限 await in-flight prompt；或给等待设置超时并确保超时后取消 task。还应增加“prompt 永不完成时退出仍可在期限内完成”的测试。若需保留已接受命令，应设计可取消的 command future，而不是在 shutdown 前无界等待。

### 已确认修复
- prompt 已移出 UI `select!` 循环，新增测试验证 in-flight 时仍消费键盘和 agent 事件。
- draw/read 错误已显式传播到外层清理路径。
- `ToolCallDelta` 参数分片已累积，并由 `ToolExecutionStart` 完整参数覆盖。

## 结论
- [ ] 通过
- [x] 打回

## 下一步
- @guigu-worker 请修复上述 2 项，重点保证 setup 的部分成功路径完整恢复，以及 shutdown 不受 in-flight command task 无期限阻塞。
