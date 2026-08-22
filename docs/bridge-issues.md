# opencode-chat-bridge 问题记录

记录当前 TypeScript bridge 的所有问题，作为 Rust 重写的参考。

## 1. 线程消息路由问题（核心问题）

**问题**: threadIsolation=true 时，所有回复都发到线程里，即使原始消息在主线程。

**根因**: sendReply/sendNoticeReply 总是创建 m.relates_to thread relation。

**修复**: 检查 context.threadRootEventId 为空时，回复也发到主线程。

## 2. Agent 间通信的 allowedUsers 问题

**问题**: Agent 发的消息被 isUserAllowed 过滤掉。

**根因**: allowedUsers 只包含 PM，不包含其他 agent。

**修复**: Worker: PM+reviewer, Reviewer: PM+worker, Planner: PM+reviewer。

## 3. ACP 权限请求的 optionId 问题

**问题**: Bridge 硬编码 "accept" 作为 optionId，但 Opencode 发送 {optionId:"once"}。

**修复**: 从 params.options 动态读取，优先 always > once > 第一个 allow。

## 4. opencode 配置文件路径问题

**问题**: Opencode 只从 .opencode/ 子目录读取配置。

**修复**: copyOpenCodeConfig 同时复制到 session root 和 .opencode/ 子目录。

## 5. qwen 上下文超限问题

**问题**: Qwen session 超过 131072 token 限制。

**根因**: Qwen 在 ~/.qwen/projects/ 下保存 session，Bridge 只删了自己 的。

**修复**: 同时清理两处 session，加 --max-session-turns 20。

## 6. 模型不遵循输出格式

**问题**: Worker 完成后不 @reviewer，Reviewer 打回后不 @worker。

**缓解**: 输出格式模板明确要求 @mention，git pre-commit hook 强制门禁。

## 7. 空响应重试

**问题**: Opencode ACP 返回空响应，Bridge 自动重试一次。

## 8. toolMessages 过滤

**问题**: 大量工具输出被过滤，用户看不到中间过程。

**根因**: showOutputFor 只配置了 ["bash"]。

## 9. 多 Bot 配置同步

**问题**: 每个 bot 有独立配置，修改后需手动同步多处。

## 10. 多工具 session 存储不一致

**问题**: Qwen/Opencode/Bridge 各自存 session，需同步清理。

---

## Rust Bridge 重写建议

### 核心改进

1. **消息路由**: 正确处理主线程 vs 线程消息
2. **Agent 通信**: 内置 agent 间 @mention 和 allowedUsers 支持
3. **Session 管理**: 统一管理所有工具的 session 生命周期
4. **ACP 集成**: 动态处理权限请求，支持 opencode/qwen/codebuddy
5. **配置管理**: 单一配置源，支持热重载
6. **错误处理**: 更好的错误恢复和重试逻辑
7. **监控**: 内置 metrics 和健康检查

### 技术栈

- **Matrix**: ruma 或 matrix-sdk
- **ACP**: JSON-RPC over stdio（tokio-process）
- **配置**: serde + toml/yaml
- **日志**: tracing
