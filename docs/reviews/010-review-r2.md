# Task 010 Review - Round 2

## 基本信息

- 审查时间: 2026-09-01
- 审查员: guigu-reviewer
- 任务规格: `docs/tasks/010-remote-protocol.md`
- 审查提交: `845f51e`

## 门禁结果

- cargo check: ✗ 未执行，当前审查环境无 `cargo`（`cargo: command not found`）
- cargo clippy: ✗ 未执行，当前审查环境无 `cargo`
- cargo test: ✗ 未执行，当前审查环境无 `cargo`
- cargo fmt: ✗ 未执行，当前审查环境无 `cargo`

## 代码审查

### 问题

1. **[Major] `src/remote/client.rs:208-214` — `abort` 未检查连接关闭状态，写 task 失败后仍可能错误返回成功**
   - 写 task 发生 IO 错误后会退出，但 `RemoteClient.tx` 的 sender 仍然存活，因此 `abort()` 仅调用 `tx.send(...)` 时仍可能成功入队；此时没有 writer 消费该请求，调用方却得到 `Ok(())`。这违反“连接关闭后后续命令返回 `RemoteError`”的约定，也与 `send_command` 的关闭检查行为不一致。
   - **建议**：在 `abort` 入队前检查 `closed`；入队后至少再检查一次关闭状态（或改为共享统一的发送辅助函数）。写失败通知与检查之间的竞态应允许返回错误，但不得在已观察到 `closed=true` 后返回 `Ok(())`。补充回归测试，写端失败后调用 `abort()` 并断言返回 `Err`。

## 建议

1. `src/remote/client.rs:61-105`、`src/remote/client.rs:108-134`：读/写 task 当前只通过 `closed` 和 pending drain 传播状态，写失败后读 task 仍可能继续读取并广播事件。建议增加连接级取消令牌，在任一方向失败时取消另一 task，并统一清理资源，避免半关闭连接继续运行。
2. `src/remote/mod.rs:204-223`：子进程回收测试依赖固定存在的 `sleep` 命令和 Linux `/proc`，跨平台执行会失败。建议按目标平台条件编译，或使用项目已有的可移植子进程/退出状态验证方式；若项目明确仅支持 Linux，应在测试模块或任务文档中注明。

## 结论

- [ ] 通过
- [x] 打回

## 下一步

@guigu-worker 请修复上述第 1 项，补充 `abort` 在连接关闭后的回归测试，并在具备 Rust 工具链的环境重新运行 `cargo check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all-targets`、`cargo fmt --check` 后申请复审。
