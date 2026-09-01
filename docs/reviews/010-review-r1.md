# Task 010 Review - Round 1

## 基本信息

- 审查时间: 2026-09-01
- 审查员: guigu-reviewer
- 任务规格: `docs/tasks/010-remote-protocol.md`
- 审查提交: `38c0270`

## 门禁结果

- cargo check: 未执行（当前审查环境未安装 `cargo`，命令返回 `cargo: command not found`）
- cargo clippy: 未执行（同上）
- cargo test: 未执行（同上）
- cargo fmt: 未执行（同上）

## 代码审查

### 问题

1. **[Critical] `src/remote/server.rs:40-73` — 初始 Snapshot 没有得到发送顺序保证**
   - `event_task` 在初始 Snapshot 入队前就被 spawn。若 runtime 在连接建立后立即产生事件，事件转发 task 可能先执行 `tx_event.send(Event)`，从而 writer 先发送 `Event`，再发送 `Snapshot{id: 0}`。这违反规格中“连接建立后立即推送一条初始 Snapshot”以及客户端基线初始化的顺序假设；客户端可能先收到增量事件，随后被旧快照覆盖，造成事件/快照状态不一致。
   - **建议**：先将 `Snapshot{id: 0}` 放入 writer 队列，再启动事件转发 task；或者使用明确的启动屏障（初始 Snapshot 写入完成后再允许事件 task 入队）。同时补充一个能在连接建立瞬间产生事件的回归测试，断言第一条服务端消息始终为初始 Snapshot。

2. **[Major] `src/remote/client.rs:108-118`、`src/remote/client.rs:139-157` — 写 task 失败不会通知客户端连接已关闭**
   - 写循环遇到 `write_line` 错误后直接退出，但没有设置 `closed`，也没有排空 `pending`。此后 `tx` 仍然存在，后续命令可以继续成功入队；由于没有 writer 消费或响应，调用方只能等待 30 秒超时。更严重的是已在途请求也不会立即失败，和规格要求“连接关闭后后续命令返回 RemoteError”及连接关闭时清理 pending 不一致。
   - **建议**：读 task 和写 task 共享统一的关闭通知/取消机制。写失败时设置 `closed=true`，排空 `pending` 与 `pending_snapshots`（最好发送明确的 oneshot 错误，而不是仅依赖 sender drop），并关闭/取消读 task；命令发送前及入队后都应检查关闭状态。补充对端关闭写端/读端和写失败场景的测试，验证不会等待完整 30 秒。

3. **[Major] `src/remote/server.rs:57-65`、`src/remote/server.rs:102-105` — writer task 出错后服务端仍继续读请求**
   - writer task 发生 IO 错误后只是退出，`tx` 仍可发送，`serve` 的读循环继续处理请求并将响应/事件放入无人消费的 unbounded channel。客户端不会收到连接关闭信号，最终表现为请求长期挂起/超时；同时 unbounded channel 可能持续积累消息，存在资源增长风险。
   - **建议**：writer 失败时通过 `watch`/oneshot 通知主读循环，立即结束 `serve` 并 abort 事件 task；或让 writer 错误通过共享状态传播到读循环。不要在 writer 已退出后继续向无界队列生产消息，并为写端关闭增加回归测试。

4. **[Warning] `src/remote/mod.rs:157-160` — `StdioStream` drop 只启动 kill，不回收子进程**
   - `Drop` 中调用 `Child::start_kill()` 后丢弃 child，没有等待 `wait()` 回收退出状态。反复创建/丢弃远程客户端时，子进程可能变成 zombie，长期运行的宿主进程会耗尽进程表资源。该问题也使 kill 失败仅被静默忽略。
   - **建议**：由 connector 管理一个异步子进程回收 task（drop 时发取消/kill 信号，task 中 `kill().await` 后 `wait().await`），或提供显式异步关闭 API 并在文档中要求调用方使用；至少增加子进程退出/回收测试。

## 建议

1. `src/remote/client.rs:81-87`：初始 Snapshot（id=0）与普通 `GetSnapshot` 的响应当前通过 id 特判区分，建议显式记录连接初始化状态；若收到重复 id=0，应记录协议错误或定义清晰的覆盖语义，避免异常服务端帧静默改变状态。
2. `src/remote/server.rs:43-54`：`Lagged` 分支直接丢弃事件。规格允许客户端通过 `GetSnapshot` 恢复，但服务端可以考虑在发生 lag 时主动推送最新 Snapshot，减少客户端观察到不连续事件流的窗口。
3. `src/remote/mod.rs:109-117`：`listen_tcp` 每次只接受一条连接并丢弃 listener，API 名称容易让调用方误以为是长期监听。建议改为返回 listener/提供 `accept` 循环封装，或在文档中明确其“一次 accept”语义。

## 结论

- [ ] 通过
- [x] 打回

## 下一步

请 @guigu-worker 修复上述第 1-3 项（至少补齐连接关闭/写失败传播与初始 Snapshot 顺序保证），并在可用 Rust 工具链环境重新运行并记录四道 DoD 门禁后申请复审。第 4 项虽不影响 loopback 测试，但在 stdio connector 的长期使用场景必须处理或明确生命周期约束。
