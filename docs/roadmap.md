# guigu 路线图（v0.1.0 之后）

> 状态：候选方向清单（待 PM 定序，未立项）
> 依据：各任务审查遗留项 + 架构预留 + 已声明边界

以下为下一阶段候选方向，每条含：**动机**（引用遗留来源）+ **大致范围** + **新依赖/风险**。排序不代表优先级，PM 定序后由 Architect 立项出规格。

## 1. ACP SSE/HTTP 远程多 client

- **动机**：014 将 `acp-sse` 降级为存根（`serve_sse`），当前 ACP 仅支持本地 stdio（1 进程 = 1 client）。补齐后支撑编辑器远程多 client。
- **范围**：实现 ACP 的 SSE + HTTP transport，复用 013 多 session 核心，远程多 client 并发接入。
- **依赖/风险**：需引入 `axum`（HTTP server）+ 可能 `reqwest`/SSE 客户端，属**新依赖，需 PM 拍板**；涉及连接生命周期、认证、多 client 会话隔离。

## 2. 持久化 lane head / 活动分支元数据

- **动机**：015 r2 遗留——当前以「最大 NodeId」推断活动叶，单 lane 边界成立，多 lane fork 场景无法表达真正活动 lane（真实功能缺口）。
- **范围**：持久化 lane head（活动分支指针）或让恢复 API 显式接收目标 head，替代「最大 NodeId」推断。
- **依赖/风险**：触及 009 session 存储 schema 与 012/013 lane 恢复语义，需向后兼容既有 JSONL 文件。

## 3. schemars 强类型工具参数

- **动机**：架构 §3.4 预留——`parameters() -> Option<serde_json::Value>` 一期宽松，升级为可生成 JSON Schema 的类型化契约。
- **范围**：引入 `schemars`，让 `Tool::parameters` 产出类型化 schema，供 ACP/插件/编辑器做参数校验与表单生成。
- **依赖/风险**：新增 `schemars` 依赖；需定义与既有 `serde_json::Value` 宽松契约的迁移/兼容策略。

## 4. Agent 插件 / 生命周期钩子

- **动机**：016 排除项——插件机制目前仅在 Tool 层（`Plugin` trait + `PluginRegistry`），扩展到 Agent 层（自定义 agent 类型、钩子注入）。
- **范围**：将插件机制从 Tool 扩展到 Agent 生命周期（before/after 钩子、自定义 agent 类型注册）。
- **依赖/风险**：需与 001 `Agent` trait + 003 主循环 `LoopConfig` 钩子对齐，边界清晰但设计面较广。

## 5. 跨进程会话锁 / 多写者文件锁

- **动机**：006/012 声明边界——`FileMutationQueue` 为进程内锁，`SharedSessionStorage` 仅进程内多 lane 串行化，跨进程多写者需文件锁层级。
- **范围**：真实分布式多进程同 session 并发写 + 同文件写，引入文件锁（如 `fs2`/`flock`）或跨进程协调。
- **依赖/风险**：新增文件锁依赖；需处理锁超时、崩溃遗留锁、跨平台语义（尤其 Windows）。

## 备注

- 权威任务索引见 `docs/TASK_BOARD.md`；架构定稿见 `docs/architecture.md`（v1.0，v0.1.0 终态同步）。
- 既定范围 002–018 已全部交付，本文件仅列下一阶段候选，未立项不进入 `TASK_BOARD.md` Backlog。
