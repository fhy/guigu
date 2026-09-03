# Task 014 Review - Round 1

## 基本信息
- 审查时间: 2026-09-03
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/014-acp-adapter.md
- 审查提交: 5570c97

## 门禁结果
- cargo check: 未执行：当前审查环境未安装 `cargo`（`cargo: command not found`）
- cargo clippy --all-targets -D warnings: 未执行：当前审查环境未安装 `cargo`
- cargo test --all-targets: 未执行：当前审查环境未安装 `cargo`
- cargo fmt --check: 未执行：当前审查环境未安装 `cargo`

## 代码审查
### 问题
1. [Critical] src/acp/handlers.rs:39-43 — `authenticate` 与规格不一致。
   - 影响: 规格方法映射（以及边界声明）要求一期不支持认证时返回 `authMethods: []`；当前对合法的 `authenticate` 请求返回 JSON-RPC 错误，兼容 ACP 客户端可能在初始化后仍调用该方法并因此中止会话。
   - 建议: 按规格返回 `{ "authMethods": [] }`（或与正式 ACP v1 的响应结构一致），并增加对应单测；不要仅依赖 initialize 中的空能力声明。

2. [Critical] src/acp/transport.rs:45-60、263-270 — JSON-RPC request/response id 被错误限制为无符号整数。
   - 影响: ACP/JSON-RPC 的 id 可以是字符串或数字；agent 发出的 client 请求虽使用 `u64`，但 client 对该请求的字符串 id 应答会被静默忽略，导致 `AcpClient::request` 永久等待。合法的负数 id 也无法路由。
   - 建议: pending 表和 `resolve_pending` 使用可哈希的完整 JSON-RPC id（建议定义只允许 string/number 的 `RequestId`，或使用规范化后的 JSON key），出站请求保留该类型；对非法/缺失 id 返回明确错误而不是忽略。补充字符串、负数及应答路由测试。

3. [Critical] src/acp/handlers.rs:106-153、src/acp/mod.rs:131-146 — 权限模式是跨 session 的全局状态，且 `session/set_mode` 未校验/使用 `sessionId`。
   - 影响: 多 session 并发时，一个 session 设置 mode 会改变其他 session 的 fs 工具行为，造成权限绕过或不必要的拒绝；调用方传入不存在的 session 也能修改全局权限状态。规格明确该方法是 session 级设置。
   - 建议: 将 mode 放入 session 级状态（按 sessionId 建立 `Arc<RwLock<PermissionMode>>`，或由 session context 携带），`set_mode` 必须解析并校验 `sessionId`，创建 `AcpFsTool` 时绑定对应 mode；增加两个 session 并发隔离测试。

4. [Warning] src/acp/transport.rs:173-184 — `request` 在 pending 表插入后发送失败时未移除 pending entry。
   - 影响: writer 关闭或发送失败后，每次失败请求都会在 `HashMap` 中遗留一个 oneshot sender；长时间运行的连接可能持续积累无效 pending，形成资源泄漏。
   - 建议: 发送失败分支中按 id 删除 pending（最好提供 `remove_pending` helper），并测试 writer 已关闭时 pending 表无残留。

5. [Warning] src/acp/transport.rs:242-289 — 连接 EOF/读错误时未取消或唤醒 in-flight handler 与 pending client 请求。
   - 影响: `session/prompt` 或 fs 工具正在等待 client response 时，client 断开后 task 仍可能永久挂起；错误路径也没有统一清理这些 task，连接资源无法及时回收。
   - 建议: 为每条连接建立 `CancellationToken` 并注入 `StdioClient`/handler；EOF、读错、writer 错误时 cancel，令 pending oneshot 全部以明确错误结束，并等待/abort handler tasks。至少增加 client 在 fs 请求等待期间断开的测试。

### 建议
1. src/acp/transport.rs:47-50 — 应校验 `jsonrpc == "2.0"`，并对同时包含 method/result、缺少 id 的非法消息返回标准 JSON-RPC 错误；当前注释明确写“一期不校验”，会把 wire 错误静默吞掉。
2. src/acp/handlers.rs:58 — 注释仍写“返回 `null`”，代码已改为返回 `{sessionId}`，应同步修正文档避免误导维护者。
3. src/acp/types.rs:61-69 — `ContentBlock` 仅声明 Text 是合理的最小面，但对 image/resource 等合法 ACP prompt 块应返回可诊断的“不支持内容类型”错误，而不是让整个请求落入泛化 serde 错误。

## 结论
- [ ] 通过
- [x] 打回

## 下一步
- @guigu-worker 请修复问题 1-3（至少），并在修复后重新运行四道门禁。
- 当前环境无法运行门禁；修复提交中请附真实的 `cargo check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all-targets`、`cargo fmt --check` 结果。
