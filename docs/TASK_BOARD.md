# TASK_BOARD.md

状态：[ ] 待做 / [~] 进行中 / [x] 完成

## Backlog

- [x] 002 — Message/Event 数据结构（基础，先行）
- [x] 001 — Agent trait + 生命周期 AgentHandle（依赖 002）
- [x] 003 — Tool trait + Runtime 执行引擎（依赖 001、002）
- [x] 004 — 最小 Echo Agent 端到端（依赖 003）

## 二期 Backlog（优先级已排定，逐个实现）

实施顺序：005 → 006 → 007 → 008 → 009 → 010

- [x] 005 — 内置文件工具 read/write/edit（ReadOnly + FileWriter，复用 003 Tool trait）
- [x] 006 — bash 工具 + file_mutation_queue（Exclusive + 跨 agent 同文件写串行化）
- [x] 007 — adapters（OpenAI/Anthropic，reqwest feature-gated）
- [x] 008 — 上下文摘要压缩 Compactor（依赖 007 真实现）
- [x] 009 — Session 树 + JSONL 崩溃恢复（r3 审查通过，四门禁全绿）
- [x] 010 — 远程协议（serde + newline-delimited JSON 双向流，r3 审查通过）
- [x] 011 — 工具惰性加载 DeferredTool（DeferredToolSpec 分离 schema 与执行体，OnceLock 惰性构建）

## 三期 Backlog（多 client / 多 lane / ACP / CLI；插件延后）

实施顺序：012 → 013 → 014 → 015

- [x] 012 — 多 lane session（SharedSessionStorage 串行化 append + LaneWriter 每 lane 游标，进程内多 lane 并发写同一 session 树）
- [x] 013 — Agent Server（多 session 注册表 + 多 lane 调度核心 + 多连接 TCP 协议）
- [x] 014 — ACP 适配（Agent Client Protocol v1：stdio 必做 / SSE 可选，session/prompt/cancel/fs/权限映射到 013）
- [x] 015 — CLI 独立运行（clap：交互式 REPL + `--acp` 模式，复用 013/007/005/006/009）

## 四期 Backlog（插件机制 + 技术债收尾）

实施顺序：016 → 017（技术债收尾待排）

- [ ] 016 — 插件机制 Plugin Registry（Plugin trait + PluginTool 异步惰性实例化 + PluginRegistry 注册表，基于 011 DeferredToolSpec，失败不缓存可重试）

## 备注

- 实施顺序：002 → 001 → 003 → 004
- 规格见 docs/tasks/NNN-xxx.md；架构定稿见 docs/architecture.md（v1.0）
- 001 规格 v1.1（2026-08-25）：定稿并发排队、事件序列、wait_for_idle 同步点+超时、reset/abort/shutdown 契约（依据 r6 审查）
- 001 已于 r7 审查通过（2026-08-26，docs/reviews/001-review-r7.md）：四门禁全绿、r6 十项阻塞全部核销
- 003 已于 r2 审查通过（2026-08-26，docs/reviews/003-review-r2.md）：四门禁全绿、r1 五项问题全部核销（含 no-Done→Error 新增测试）
- 004 规格 v1.3（2026-08-26，Architect 三次重核验）：Developer 预实现审查发现 v1.2 正文误记工具注册契约——工具注册在 003 定稿中已落 `AgentRuntime.tools`（非 `AgentConfig`），spawn 为双参；本次修正第 28/35 行，工具经 `AgentRuntime { tools }` 注册 + 双参 spawn，不再给 `AgentConfig` 加 `tools` 字段（消除双重事实源）。旧实现（8-22，567265b）早于 001/003 定稿已过期，需按 v1.3 重做
- 004 已于 r1 审查通过（2026-08-27，docs/reviews/004-review-r1.md）：四门禁全绿、40 测试通过；EchoTool 签名与 003 定稿一致，工具经 `AgentRuntime.tools` + 双参 spawn
- 一期（002/001/003/004）全部完成并审查通过，核心运行时 + 最小端到端闭环
- 二期优先级依据（2026-08-27，Architect 排定）：005 文件工具最基础、零外部依赖，直接复用 003 Tool trait + 004 的 src/tools 结构；006 bash（Exclusive 验证独占编排）+ file_mutation_queue（跨 agent 写串行化安全底座）；007 adapters 接真实 LLM（fake provider → 生产）；008 压缩真实现需调用 LLM 摘要，故在 007 之后；009 session 持久化独立但价值次于"接真实模型+可摘要"；010 远程协议最外层最后
- 006 规格 v1.1（2026-08-27，Architect，依据 Developer 预审 r1 修订）：FileMutationQueue 为进程内 per-path 异步写锁（RAII guard，跨 agent 同文件串行化），WriteTool/EditTool 改为 `new(Arc<FileMutationQueue>)` 注入；BashTool 声明 Exclusive（单 agent 独占由 003 主循环保证）、`sh -c` 子进程 + kill_on_drop、非零退出走 `ToolResult::is_error` 不 throw。明确边界：跨进程串行化、bash 跨 agent 独占（需全局读写锁层级）不在本任务。tokio `full` 已含 process/time，无需改 Cargo.toml。r1 修订三处：① 三路 select 弃 `wait_with_output`（按值消费 Child → E0382），改 `child.wait()`（&mut）+ 提前 take stdout/stderr + spawn 排空；② 取消/超时 `kill().await` 后 `wait().await` 严格 reap（不依赖 best-effort reaper）；③ 锁表一期只增不减（安全驱逐需两阶段 dying 态，后续补，声明为已知局限）
- 006 已于 r1 审查通过（2026-08-27，docs/reviews/006-review-r1.md）：四门禁全绿、84 测试通过（23 lib + 10 bash + 1 queue + 14 tools + 36 既有）；v1.1 三处修订（wait_with_output→child.wait()、kill+wait 严格 reap、锁表只增不减）均正确落地，FileMutationQueue 惰性建锁 + OwnedMutexGuard 设计符合规格建议
- 007 规格 v1.0（2026-08-27，Architect）：复用 003 定稿 ModelProvider/AssistantStream/AssistantEvent/ProviderRequest（不改签名）；ProviderError 四类语义（Network/HttpStatus/Parse/Build，若 003 缺变体则补齐）；default feature 含 providers-http（保证 DoD `cargo test` 覆盖 adapter，嵌入方 default-features=false 剥离 reqwest）；SSE/请求构造/事件映射/累积四层纯逻辑 + wiremock 端到端测试；Model/Context 具体字段形状以 core/provider.rs 003 实际实现为准（语义固定，见规格映射表）
- 007 已于 r5 审查通过（2026-08-27，docs/reviews/007-review-r5.md）：四门禁全绿、180 测试通过（含 `--no-default-features` 75 通过，验证 default-features=false 可剥离 reqwest）；r2/r3/r4 打回项（重复 block index / 重复 stop / OpenAI 错误路径状态一致）均核销，r5 无阻塞问题
- 008 已于 r2 审查通过（2026-08-28，docs/reviews/008-review-r2.md）：四门禁全绿、r1 三项问题全部核销；规格 v1.1 消除正文与伪代码矛盾
- 009 规格 v1.0（2026-08-28，Architect）：落定 architecture 3.8 预留 `SessionStorage` trait；树用 parent_id 指针隐式表达（fork=任意历史节点追加）；append 为 O(1) 追加不校验结构、结构校验集中 reduce；崩溃恢复=逐行解析跳半行+全量重放+next_id 续写恢复；sync_all 保证进程崩溃级持久性（不保证断电）；单 writer 边界、多 lane 并发属 010；提供可选 SessionRecorder 桥接复用 001 subscribe() 事件流，不改 003 主循环
- 009 已于 r3 审查通过（2026-08-31，docs/reviews/009-review-r3.md）：四门禁全绿、139 测试通过；r1（id 溢出 / path_to 叶契约）→ r2（单文件 408 行超限）→ r3 无阻塞问题，`session.rs` 已拆分 `JsonlSessionStorage` 至 `jsonl.rs` 子模块
- 010 规格 v1.0（2026-08-31，Architect）：跨进程远程协议 = serde + newline-delimited JSON 在线双向流；命令面与 001 `AgentCommand` 一一对应；`RemoteServer`（serve 一条连接）/ `RemoteClient`（watch+broadcast 本地重建进程内契约）；connector 复用同一 codec（stdio/tcp）；连接即推初始快照对齐「lag→重读 snapshot」；单 agent 边界、多 lane 并发写 session 排除（后续任务）；无新增依赖
- 010 已于 r3 审查通过（2026-09-01，docs/reviews/010-review-r3.md）：r1（初始 Snapshot 顺序 / 连接关闭传播 / writer 失败传播 / 子进程回收）→ r2（abort 关闭检查）→ r3 无阻塞，164 测试通过。注：审查环境无 cargo，四门禁以 Developer 执行记录（164 全绿）为准，非 reviewer 独立复跑——建议 PM 在具备工具链环境补一次独立门禁复核
- 011 规格 v1.0（2026-09-01，Architect）：补 architecture 二期 deferred tools 缺口。DeferredToolSpec（owned schema：name/description/parameters/resource_scope）与执行体分离，DeferredTool 实现 Tool trait 惰性包装——schema 方法只读 spec 不触发工厂，execute 首次经 `std::sync::OnceLock` 构建并缓存（进程内仅一次，不跨 await 持锁）；工厂同步 + infallible，async 实例化留后续「插件」任务。零破坏：DeferredTool 本身是合法 Tool，仍入 `Vec<Arc<dyn Tool>>`，不改 003 主循环与注册契约
- 011 规格 v1.1（2026-09-01，Architect，依据 Developer 架构审查）：新类型 `ToolSpec` 改名 `DeferredToolSpec`，消除与既有 `core::provider::ToolSpec`（003/007 定稿 wire 格式）顶层 glob 重导出撞名歧义。方案 A（改名不动 provider 侧），零破坏
- 三期拆分（2026-09-01，Architect，依据 PM 定稿意见「多 client / 多 lane session / CLI 独立运行 / ACP，插件延后」）：012 多 lane session（底层）→ 013 Agent Server（多 session 注册表 + 多 lane 调度核心 + 多连接 TCP）→ 014 ACP 适配（Agent Client Protocol v1，stdio 必做 / SSE 可选）→ 015 CLI（clap）。关键决策见 architecture.md §7：ACP 为对外标准协议、010 协议保持单连接不扩展、多 lane 仅进程内、插件延后（011 为前置）
- 011 已于 r1 审查通过（2026-09-01，docs/reviews/011-review-r1.md）：四门禁全绿、170 单测 + 集成测试通过，无阻塞问题。非阻塞建议：b872ecc 混入 docs/ 规格与历史审查文件，后续 Developer 提交只含 src/tests，按角色边界拆分。下一动作：启动 012 开发（规格 v1.0 已就绪）
- 012 已于 r1 审查通过（2026-09-01，docs/reviews/012-review-r1.md）：代码审查无阻塞问题（SharedSessionStorage 全程持 tokio Mutex 串行化 append、LaneWriter 独立 head 游标、fork 分支语义与 009 reduce 一致），r1 三条非阻塞建议（inner() footgun、LaneWriter 未强制共享写入口、并发测试未覆盖每 lane 多步写）留待后续。注：审查环境无 cargo，四门禁以 Developer 执行记录（全绿）为准，非 reviewer 独立复跑——建议 PM 在具备工具链环境补一次独立门禁复核。下一动作：启动 013 开发（规格 v1.0 已就绪）
- 013 规格 v1.0（2026-09-01，Architect）：多 session 注册表（create/load/close）+ 多 lane 调度核心（spawn_lane/fork_lane/abort/shutdown）+ 多连接 TCP 协议（server/protocol/transport/lane）；复用 012 SharedSessionStorage 与 009 持久化，协议面复用 010 codec 思路（NDJSON）；无新增依赖
- 013 已于 r2 审查通过（2026-09-02，docs/reviews/013-review-r2.md）：r1 三项 Critical/High（spawn_lane/fork_lane 入表非原子竞态、Shutdown 未走全局语义、重复 Subscribe 未取消旧 forwarder）均已核销，新增回归测试断言有效结果；四门禁全绿、198 lib 测试 + server 9/9 集成通过。下一动作：启动 014 开发（规格 v1.0 已就绪）
- 014 已于 r4 审查通过（2026-09-03，docs/reviews/014-review-r4.md）：四门禁全绿、243 单测 + 集成测试全绿（含 tests/acp.rs）；r1→r4 三轮打回项（authenticate/RequestId/session 级 mode、writer 错误通知读循环、FailingWriter Pin 签名/Tool trait 导入等）全部核销。非阻塞建议：tests.rs(476)/tests_transport.rs(413) 略超 400 行单文件上限（后续再拆）。规格措辞已由 Architect 于 v1.1 统一（cancellation → cancelled，对齐官方 wire）。下一动作：启动 015 开发（规格 v1.0 已就绪）
- 015 已于 r2 审查通过（2026-09-04，docs/reviews/015-review-r2.md）：四门禁全绿、245 库测试 + CLI 7 通过；r1 Critical（`--session` 续聊仅 load_session 未恢复 transcript，且 LaneWriter head 为 None 致新消息成新根）已核销（resume_lane_from_factory 重载 session 树取最新叶 + 注入 transcript 到 snapshot/runtime + 设 LaneWriter head）。非阻塞建议两条：① 活动叶以最大 NodeId 推断在单 lane 边界成立，多 lane fork 时无法表达真正活动 lane，后续需持久化 lane head/活动分支元数据或让恢复 API 显式收目标 head；② `set_current_dir` 改进程级 cwd，多 session 场景应显式传工作目录给工具配置。三期（012/013/014/015）全部完成
- 四期启动（2026-09-04，Architect）：016 插件机制为四期开篇，补齐 architecture §7.2「插件延后」与 011 修订记录「async 实例化属后续插件任务」的双重缺口。规格 v1.0（docs/tasks/016-plugin-registry.md）：`Plugin` trait（id + owned schema 集合 + async 可失败 instantiate）+ `PluginTool`（`tokio::sync::OnceCell::get_or_try_init` 异步惰性、失败不缓存可重试）+ `PluginRegistry`（std RwLock、确定性工具组装）。边界明确排除：动态库 dlopen、Agent 插件、插件生命周期钩子、跨进程/远程加载。后续 017 计划技术债收尾（打包 014/015/006/012 遗留非阻塞项，待 PM 定序）
- 016 待 Developer 实现（规格 v1.0 已就绪，等待 PM 启动开发）
