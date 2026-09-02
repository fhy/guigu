# Task 013 Review - Round 1

## 基本信息

- 审查时间: 2026-09-02
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/013-agent-server.md
- 审查提交: c5ba05d

## 门禁结果

- cargo check: ✓
- cargo clippy -- -D warnings: ✓
- cargo test: ✓（198 个 lib 测试，全部集成测试通过）
- cargo fmt --check: ✓

## 代码审查

### 问题

1. **[Critical] src/server/lane.rs:33-66、src/server/lane.rs:85-125 — 入表前检查与最终插入不是原子操作**
   - 影响: 两个并发 `spawn_lane("s", "l")` 都可能通过第一个锁区间的 `contains_key` 检查，随后分别 spawn runtime，最后后写入的 lane 覆盖先写入的 lane；前一个 runtime 及其持久化 bridge 失去句柄且无法 shutdown。`fork_lane` 同样存在该竞态。此外，`shutdown`/session 变更与入表之间也可能导致 spawn 成功但 lane 根本没有登记，返回值仍为 `Ok(())`。
   - 建议: 保持 runtime spawn 在锁外，但第二次获取注册表锁后必须再次校验 session 存在及 lane 不存在；校验失败时显式 shutdown 已 spawn 的 handle 并返回 `SessionNotFound`/`LaneAlreadyExists`。更稳妥的方案是引入 reservation（先在表中预留 lane，失败时清理），并为 `fork_lane` 做同样的二次校验，确保不会覆盖已有运行时。

2. **[High] src/server/transport.rs:323-331 — `Shutdown` 请求没有调用 `AgentServer::shutdown()`**
   - 影响: 协议定义 `ServerRequest::Shutdown` 的语义是“关闭 server（所有 session / lane）”，但当前实现仅回复成功并关闭当前连接。发送 Shutdown 后，所有 session/lane 仍存活，和 wire 文档及 `AgentServer::shutdown` 门面语义不一致；客户端会收到一个表示成功的响应，却没有得到请求的全局关闭效果。
   - 建议: 在发送成功响应后调用 `server.shutdown().await`，再关闭当前连接；或如果产品明确只希望关闭连接，则应修改协议文档/变体命名为 CloseConnection，并补充跨客户端的行为测试。按当前 Task 013 规格，应实现前者，并处理 shutdown 错误。

3. **[High] src/server/transport.rs:277-287 — 重复 Subscribe 未取消旧的 forwarder**
   - 影响: 对同一 `(session_id, lane_id)` 重复订阅时，`subscriptions.insert` 会丢弃旧 token，但旧事件转发 task 仍运行。之后每个事件会被发送两次，`Unsubscribe` 只取消新 task，旧 task 会一直存活到连接关闭，造成重复事件和 task 泄漏（可反复订阅放大）。
   - 建议: 插入新 token 前对 `subscriptions.remove(...).map(|old| old.cancel())`；或者重复 Subscribe 直接返回协议错误。增加重复 Subscribe → 单事件只收到一次、Unsubscribe 后不再收到事件的测试。

### 建议

1. `src/server/transport.rs:70-85` — 当前 writer task 失败通过 `watch` 通知读循环，但 `watch::Receiver::changed()` 在 writer 正常退出且未发送失败时也可能以 channel closed 返回错误；建议明确区分 writer 正常关闭与写失败，并让连接关闭时可靠地终止所有订阅 task。
2. `src/server/tests.rs:88-136`、`tests/server.rs:120-154` — 轮询辅助函数使用 `expect`/`panic`，虽然仅在测试代码中且产品代码无 `unwrap`，但可改成带超时的 `Result`，使失败原因更易组合和诊断。
3. `src/server/mod.rs:123-140` — `with_runtime_factory` 与 `with_storage_factory` 的重复设置静默忽略，调用者无法发现配置错误；建议返回 `Result` 或至少记录 warning。若 API 兼容性要求保留当前签名，应在文档中明确首次设置优先的不可变配置语义。

## 结论

- [ ] 通过
- [x] 打回

## 下一步

@guigu-worker 请修复上述 3 个 High/Critical 问题，重点补充并发重复 spawn、Shutdown 全局语义、重复 Subscribe 的回归测试；修复后重新运行四道 DoD 门禁并申请 Round 2 复审。
