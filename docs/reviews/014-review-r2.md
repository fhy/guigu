# Task 014 Review - Round 2

## 基本信息
- 审查时间: 2026-09-03
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/014-acp-adapter.md
- 审查提交: e117e83

## 门禁结果
- cargo check: 未执行：审查环境未安装 `cargo`（`cargo: command not found`）
- cargo clippy --all-targets -D warnings: 未执行：审查环境未安装 `cargo`
- cargo test --all-targets: 未执行：审查环境未安装 `cargo`
- cargo fmt --check: 未执行：审查环境未安装 `cargo`

## 代码审查
### 问题
1. [Critical] `src/acp/transport.rs:393-408`、`425-442` — writer task 发生写错误时没有通知连接读循环，也没有 `cancel_all`。
   - 影响: `write_line` 失败后 writer task 直接退出，但 `serve_connection` 仍可能阻塞在 `reader.next()`；已经发出的 `AcpClient::request` 会一直等待 pending oneshot。上一轮 Issue 5 明确要求 writer 错误路径也取消并唤醒 in-flight 请求，当前只覆盖 EOF/读错误。
   - 建议: writer task 将首个写错误通过共享状态/关闭通知传递给读循环；读循环 select 该错误通知与输入，统一执行 `shutdown.cancel()`、`client.cancel_all()`、handler task 回收，并返回明确的 IO/连接错误。为 writer 写失败且存在 pending request 增加回归测试。

2. [Warning] `src/acp/transport.rs:414`、`433`、`442` — 每个请求的 `JoinHandle` 永久保存在 `Vec`，直到连接关闭才回收。
   - 影响: 长连接上大量短请求会持续积累已完成 task 的句柄和任务分配，形成无界内存增长；这不是连接 EOF 才能释放的资源。
   - 建议: 使用 `tokio::task::JoinSet` 并在主循环中持续 `try_join_next()` 清理已完成任务，或让独立 supervisor 负责回收；连接关闭时再 abort 尚未完成的任务。

3. [Warning] `src/acp/transport.rs:294-304` — 缺少 `jsonrpc` 字段的消息会被接受。
   - 影响: JSON-RPC 2.0 请求/响应必须带 `jsonrpc: "2.0"`；当前注释和实现只在字段存在时校验，因此缺字段的非法 frame 可能被 dispatch，协议兼容性和错误诊断不一致。
   - 建议: 将 `jsonrpc` 反序列化为必需字段，或在 `classify_inbound` 对 `None` 返回 `-32600`，并补充缺失版本字段的测试。

4. [Warning] `src/acp/transport.rs:454-483` — handler task 仅在 EOF/读错误时回收，正常长期运行期间不会回收。
   - 影响: 与问题 2 相同，长时间运行的 stdio 连接会积累 task 句柄；此外已完成 task 的 panic 也不会被观察到。
   - 建议: 采用 `JoinSet`/后台回收机制，同时记录或处理 `JoinError`，避免任务异常静默。

5. [Warning] `src/acp/transport.rs` — 文件当前 497 行，超过项目规范单文件 400 行上限。
   - 影响: 本次修复把 transport 从 302 行扩展到 497 行，已经违反 `docs/conventions.md:280-287` 和任务验收标准 `docs/tasks/014-acp-adapter.md:122`；维护和进一步测试会继续放大复杂度。
   - 建议: 将 JSON-RPC 类型/分类、pending client、stdio connection loop 拆成职责独立的子模块，保持每个文件不超过 400 行，并更新模块注释。

6. [Warning] `src/acp/tests.rs` 当前包含超过 30 个测试，违反项目测试文件体量限制。
   - 影响: `docs/conventions.md:285` 要求单个测试文件最多 30 个 `#[test]`；当前已有 38 个测试属性，新增覆盖进一步使文件超限。
   - 建议: 将 transport/framing/request-id 测试移至 `src/acp/transport/tests.rs`（或独立集成测试），将 handler/fs/mapping 测试分别拆分，保持单文件不超过 30 个测试。

## 建议
1. `src/acp/handlers.rs:124-127` — `session/update` 通知失败被 `let _ =` 丢弃。建议在 client 断开时立即结束 prompt，并将通知错误转成 `AcpError`，避免客户端已断开但 prompt 继续消耗 runtime。
2. `src/acp/transport.rs:233-235` — `u64` 转 `i64` 使用 `as`，超过 `i64::MAX` 会回绕并可能造成请求 id 冲突。建议统一使用无损 JSON number/id 表示，或在溢出时返回明确错误。

## 结论
- [ ] 通过
- [x] 打回

## 下一步
- @guigu-worker 请修复问题 1（必须）以及问题 2-6，并重新运行四道门禁后提交复审。
