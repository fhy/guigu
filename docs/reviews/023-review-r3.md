# Task 023 Review - Round 3

## 基本信息
- 审查时间: 2026-09-10
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/023-tui.md
- 审查提交: 5e41413

## 门禁结果
- cargo check: ✓
- cargo clippy --all-targets -- -D warnings: ✓
- cargo test --all-targets: ✓
- cargo test --features tui --all-targets: ✓（320 库测试、53 binary 测试及集成测试通过）
- cargo test --no-default-features: ✓（225 库测试及集成测试通过）
- cargo fmt --check: ✓

## 代码审查
### 问题
1. [Major] `src/bin/guigu/tui/mod.rs:231-237` — abort command task 后，`server.shutdown().await` 仍可能在满 agent 队列下永久阻塞。
   - `AgentHandle::shutdown` 先执行 `self.tx.send(Shutdown).await`，该发送没有超时；若容量 100 的队列已满且当前 provider stream 永不产生下一项，runtime 无法 drain 命令，Shutdown 也无法入队。取消第 101 个、尚未入队的 prompt 不会释放前 100 个已入队命令。
   - 影响：r2 指出的“满队列 + provider/run 长时间不返回”退出挂起仍存在，Esc、第二次 Ctrl-C 及错误退出路径不能保证完成。
   - 建议：为 shutdown 建立不受 bounded data queue 背压影响的取消/控制通道，或至少在 TUI 退出路径对 shutdown 设置明确超时并确保超时后释放/终止 runtime；同时覆盖真实 `AgentServer` 满队列场景。

2. [Major] `src/bin/guigu/tui/loop_tests.rs:280-328` — 新测试没有复现其注释声称的“满队列 + `server.prompt().await` 阻塞”。
   - 测试使用空 `AgentServer`，`server.shutdown()` 立即返回；永久 pending 的只是一个与 server 无关的 fake task。因此它只能证明 `JoinHandle::abort` 可取消普通 task，无法验证真实 shutdown 路径。
   - 影响：测试在上述生产缺陷仍存在时保持绿色，属于回归覆盖缺口。
   - 建议：创建真实 session/lane，使用永不产出事件的 provider 启动 active run，填满 100 个 agent command 槽位，再断言完整退出 helper 在期限内结束；测试需先用确定性信号确认 provider 已进入阻塞、额外 prompt 已受背压。

### 已确认修复
- `setup_terminal` 已拆分 EnterAlternateScreen 与 Hide，并在各失败阶段恢复此前完成的终端状态。
- command task 在退出时会被 abort，尚未入队的 in-flight prompt 不再阻止进入 shutdown。

## 结论
- [ ] 通过
- [x] 打回

## 下一步
- @guigu-worker 请修复真实 `AgentServer` 满队列时 shutdown 仍可能挂起的问题，并以真实 server/provider 场景补齐回归测试。
