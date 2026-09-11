# guigu 路线图（v0.1.0 之后）

> 状态：候选方向清单（已全部立项并交付：024/025/027/028/029，详见各节）
> 依据：各任务审查遗留项 + 架构预留 + 已声明边界

以下为下一阶段候选方向，每条含：**动机**（引用遗留来源）+ **大致范围** + **新依赖/风险**。PM 已定序（2026-09-06）：持久化 lane head（024）与 ACP SSE/HTTP（025）**已立项并插入到 022/023 自定义模型+TUI 之前**（实施顺序 024 → 025 → 022 → 023）；`axum` 已由 PM 签核「批准 feature-gated」。剩余三条候选（schemars / Agent 插件 / 跨进程锁）已于 2026-09 全部立项：PM 定序「插件最后」，Architect 排定 **027 schemars → 028 跨进程锁 → 029 Agent 插件**（理由见 TASK_BOARD 备注十期启动记录）。

## 1. 持久化 lane head / 活动分支元数据 ✅ 已立项 024

- **动机**：015 r2 遗留——当前以「最大 NodeId」推断活动叶，单 lane 边界成立，多 lane fork 场景无法表达真正活动 lane（真实功能缺口）。
- **范围**：持久化 lane head（活动分支指针），替代「最大 NodeId」推断。
- **依赖/风险**：触及 009 session 存储 schema 与 012/013 lane 恢复语义，需向后兼容既有 JSONL 文件；无新依赖。
- **状态**：规格 v1.0 已就绪（docs/tasks/024-lane-head-persistence.md）。

## 2. ACP SSE/HTTP 远程多 client ✅ 已立项 025

- **动机**：014 将 `acp-sse` 降级为存根（`serve_sse`），当前 ACP 仅支持本地 stdio（1 进程 = 1 client）。补齐后支撑编辑器远程多 client。
- **范围**：实现 ACP 的 SSE + HTTP transport，复用 013 多 session 核心，远程多 client 并发接入。
- **依赖/风险**：`axum`（HTTP server）+ `tokio-stream`（SSE 流 helper），feature-gated 在 `acp-sse`（非 default，PM 签核）；`reqwest` 仅 dev-dep。涉及连接生命周期、认证、多 client 会话隔离（auth/TLS 一期不做）。
- **状态**：规格 v1.0 已就绪（docs/tasks/025-acp-sse-http.md）。

## 3. schemars 强类型工具参数 ✅ 已立项 027

- **动机**：架构 §3.4 预留——`parameters() -> Option<serde_json::Value>` 一期宽松，升级为可生成 JSON Schema 的类型化契约。
- **范围**：引入 `schemars`，让 `Tool::parameters` 产出类型化 schema，供 ACP/插件/编辑器做参数校验与表单生成。
- **依赖/风险**：新增 `schemars` 依赖；需定义与既有 `serde_json::Value` 宽松契约的迁移/兼容策略。
- **状态**：规格 v1.0 已就绪（docs/tasks/027-schemars-tool-params.md）。feature `schema` default 开启；内置工具参数 derive 化；零破坏 `Tool` trait（Value→RootSchema 往返替代双事实源）。

## 4. Agent 插件 / 生命周期钩子 ✅ 已交付 029（最后）

- **动机**：016 排除项——插件机制目前仅在 Tool 层（`Plugin` trait + `PluginRegistry`），扩展到 Agent 层（自定义 agent 类型、钩子注入）。
- **范围**：将插件机制从 Tool 扩展到 Agent 生命周期（before/after 钩子、自定义 agent 类型注册）。
- **依赖/风险**：需与 001 `Agent` trait + 003 主循环 `LoopConfig` 钩子对齐，边界清晰但设计面较广。
- **状态**：✅ 已交付（029，docs/tasks/029-agent-plugin-hooks.md）。PM 定序最后；`LifecycleHooks` + `AgentFactory` + `AgentPluginRegistry`，零破坏 001/016/003。

## 5. 跨进程会话锁 / 多写者文件锁 ✅ 已立项 028

- **动机**：006/012 声明边界——`FileMutationQueue` 为进程内锁，`SharedSessionStorage` 仅进程内多 lane 串行化，跨进程多写者需文件锁层级。
- **范围**：真实分布式多进程同 session 并发写 + 同文件写，引入文件锁（如 `fs2`/`flock`）或跨进程协调。
- **依赖/风险**：新增文件锁依赖；需处理锁超时、崩溃遗留锁、跨平台语义（尤其 Windows）。
- **状态**：规格 v1.0 已就绪（docs/tasks/028-cross-process-lock.md）。`fs2` 内核态锁（崩溃自释放化解遗留锁）；`FileMutationQueue`/`JsonlSessionStorage` 可选叠加，零破坏默认行为。

## 备注

- 权威任务索引见 `docs/TASK_BOARD.md`；架构定稿见 `docs/architecture.md`（v1.0，v0.1.0 终态同步）。
- 既定范围 002–029 已全部交付（含十期 027/028/029），权威索引与状态以 `TASK_BOARD.md` 为准。
