# Task 029: Agent 插件 / 生命周期钩子

## Background

016 交付的插件机制（`Plugin` trait + `PluginRegistry` + `PluginTool`）**只覆盖 Tool 层**，明确排除了「Agent 插件、生命周期钩子」。003 定稿的主循环 `LoopConfig` 已内置一批钩子（`before_tool_call`/`after_tool_call`/`should_stop_after_turn`/`prepare_next_turn`/`convert_to_llm`/`transform_context`），但它们是**编译期注入的闭包**，无法由插件在运行期动态注册。

roadmap 候选 4 立项（PM 定序：最后做）。本任务把插件机制从 Tool 层扩展到 **Agent 层**：① 将主循环生命周期钩子抽象为可插拔的 `LifecycleHooks` trait；② 支持插件注册**自定义 agent 类型**（`AgentFactory`）。零破坏 016 工具插件与 001 `Agent` trait 签名。

## Goal

- 定义 `LifecycleHooks` trait：主循环生命周期钩子的 trait 化抽象（默认空实现），对齐 003 `LoopConfig` 钩子语义
- 定义 `AgentFactory` trait：按 id 产出 `Arc<dyn Agent>` 的工厂；定义 `AgentPlugin` trait：`id` + 贡献 hooks + 可选贡献 agent 工厂
- 定义 `AgentPluginRegistry`：注册/卸载/查询/合并 hooks / 按 id 取工厂
- **零破坏**：不改 001 `Agent` trait、不改 016 `Plugin` trait；`LoopConfig` 钩子与 `LifecycleHooks` 通过桥接对齐

## Design Notes

### 契约复用（勿改）

- `Agent` trait（001 定稿，core/agent.rs）：`snapshot`/`subscribe`/`prompt`/`continue_`/`steer`/`follow_up`/`abort`/`wait_for_idle`。
- `LoopConfig` 钩子（003 定稿，core/runtime.rs）：`convert_to_llm`/`transform_context`/`before_tool_call`/`after_tool_call`/`should_stop_after_turn`/`prepare_next_turn`/`tool_execution`。**具体字段签名以 003 定稿实际实现为权威**（语义固定，见下方映射表），本规格不重复伪代码。
- `Plugin` trait / `PluginRegistry` / `PluginTool` / `DeferredToolSpec`（016/011 定稿）：**不动**。Agent 插件为独立并行机制，不与工具插件耦合。

### LifecycleHooks（src/plugin/hooks.rs 或 src/core/hooks.rs）

> 钩子语义对齐 003 `LoopConfig`，签名形状以 003 实际为准（下表为语义映射，非逐字签名）。

```rust
/// Agent 主循环生命周期钩子。所有方法默认空实现，插件按需覆盖。
#[async_trait]
pub trait LifecycleHooks: Send + Sync {
    /// 工具执行前（对齐 LoopConfig::before_tool_call）。
    async fn before_tool_call(&self, ctx: &HookContext, args: &serde_json::Value)
        -> Result<(), HookError> { Ok(()) }
    /// 工具执行后（对齐 LoopConfig::after_tool_call）。
    async fn after_tool_call(&self, ctx: &HookContext, result: &ToolResult)
        -> Result<(), HookError> { Ok(()) }
    /// 本轮是否提前结束（对齐 LoopConfig::should_stop_after_turn）；None = 不干预。
    fn should_stop_after_turn(&self, ctx: &HookContext) -> Option<bool> { None }
    /// 下一轮准备（对齐 LoopConfig::prepare_next_turn）。
    async fn prepare_next_turn(&self, ctx: &HookContext) -> Result<(), HookError> { Ok(()) }
}
```

**对齐映射表**（语义固定；字段/参数形状以 003 定稿实际为准）：

| 003 LoopConfig 钩子 | LifecycleHooks 方法 | 备注 |
|---|---|---|
| `before_tool_call` | `before_tool_call` | 一期必做 |
| `after_tool_call` | `after_tool_call` | 一期必做 |
| `should_stop_after_turn` | `should_stop_after_turn` | 一期必做 |
| `prepare_next_turn` | `prepare_next_turn` | 一期必做（可选，若 003 有则纳入） |
| `convert_to_llm` / `transform_context` | 不在 `LifecycleHooks` 一期 | 声明为边界：上下文投影/裁剪属核心逻辑，不开放为插件钩子，避免插件破坏上下文一致性 |

- `HookContext`：钩子上下文结构体（承载当前 transcript 快照、本轮消息、当前 tool_call 等），**字段以 003 主循环实际可用状态为准**，本规格固定语义（钩子只读上下文、可返回 HookError 中断该工具/该轮，不得直接改 transcript）。
- **错误语义**：钩子返回 `HookError` 时的传播策略——`before_tool_call` 失败 = 该工具调用按失败处理（进 `ToolError` 语义，不执行工具体）；`after_tool_call` 失败 = 记录进 result，不阻断主循环；`should_stop_after_turn` 返回 `Some(true)` = 本轮后停止。**以 003 主循环既有的钩子失败处理为准**（语义固定，实现对齐）。

### AgentFactory / AgentPlugin（src/plugin/agent.rs）

```rust
/// 自定义 agent 类型工厂：按 id 构建一个 Agent 实例。
#[async_trait]
pub trait AgentFactory: Send + Sync {
    fn id(&self) -> &str;
    /// 构建 agent。`config` 为 001 定稿的 AgentConfig，`hooks` 为合并后的生命周期钩子。
    async fn build(&self, config: AgentConfig, hooks: Arc<dyn LifecycleHooks>)
        -> Result<Arc<dyn Agent>, AgentPluginError>;
}

/// Agent 层插件：贡献生命周期钩子 + 可选的自定义 agent 工厂。
pub trait AgentPlugin: Send + Sync {
    fn id(&self) -> &str;
    /// 贡献的生命周期钩子；None = 不贡献。
    fn hooks(&self) -> Option<Arc<dyn LifecycleHooks>>;
    /// 贡献的自定义 agent 工厂；None = 不贡献。
    fn agent_factory(&self) -> Option<Arc<dyn AgentFactory>>;
}
```

- `AgentPlugin` 为**独立新 trait**（不改 016 `Plugin`）：一个插件可同时实现 `Plugin`（工具）+ `AgentPlugin`（Agent 层），但两个 trait 各自独立注册、互不耦合。
- `AgentFactory::build` 产出 `Arc<dyn Agent>`（001 定稿 trait 对象），**不改 001 `Agent` 签名**。

### AgentPluginRegistry（src/plugin/agent.rs）

```rust
pub struct AgentPluginRegistry {
    plugins: std::sync::RwLock<HashMap<String, Arc<dyn AgentPlugin>>>,
}

impl AgentPluginRegistry {
    pub fn new() -> Self;
    pub fn register(&self, p: Arc<dyn AgentPlugin>) -> Result<(), AgentPluginError>; // 重复 id → DuplicateAgentPlugin
    pub fn unregister(&self, id: &str) -> Option<Arc<dyn AgentPlugin>>;
    pub fn get(&self, id: &str) -> Option<Arc<dyn AgentPlugin>>;
    pub fn list(&self) -> Vec<String>;                       // 按 id 字典序稳定排序
    /// 合并所有插件 hooks 为单个 LifecycleHooks（按 id 字典序链式调用，任一失败即短路）。
    pub fn merged_hooks(&self) -> Option<Arc<dyn LifecycleHooks>>;
    /// 按 id 取 agent 工厂。
    pub fn agent_factory(&self, id: &str) -> Option<Arc<dyn AgentFactory>>;
}
```

- 用 `std::sync::RwLock`（同 016，短临界区无 await）；`merged_hooks` 返回的组合钩子**在锁外**调用各插件 hook（只复制 `Arc<dyn AgentPlugin>` 后释放锁，避免外部回调置于注册表锁内——对齐 016 r1 教训）。
- `merged_hooks` 组合语义：按 id 字典序依次调用各插件的同方法；`before_tool_call`/`after_tool_call`/`prepare_next_turn` 任一返回 `Err` 即短路（后续插件不调用）；`should_stop_after_turn` 首个 `Some(true)` 即停。
- `unregister` 语义：只阻止新查询/新合并；已分发出去的 `Arc<dyn AgentPlugin>` 仍可调用（Arc 延长生命周期），doc 注释写明（对齐 016）。

### 与 LoopConfig 的桥接

- 提供从 `Arc<dyn LifecycleHooks>` 到 003 `LoopConfig` 钩子的适配（或让 `LoopConfig` 新增一个可选的 `hooks: Option<Arc<dyn LifecycleHooks>>` 字段，由主循环在既有钩子调用点优先查 trait 钩子）。**以 003 定稿实际调用点为权威**，语义固定：插件钩子与既有闭包钩子共存时，插件钩子先执行、闭包钩子后执行（或反之，二选一并记录）。
- CLI（015）/Server（013）装配时：`merged_hooks()` → 注入 LoopConfig；`agent_factory(id)` → 按名选用自定义 agent 类型。**本任务交付原语 + 单测，不改 015/013 装配逻辑**（声明为边界，避免范围膨胀）。

### 边界声明（明确不做）

- 动态库加载（dlopen / .so / .dylib）：不做（同 016，插件 = 进程内 trait 对象注入）。
- 不改 001 `Agent` trait、016 `Plugin` trait、003 `LoopConfig` 既有闭包钩子字段（仅新增可选桥接，不删除旧字段）。
- `convert_to_llm` / `transform_context` 不开放为插件钩子（上下文投影/裁剪属核心，开放会破坏一致性）。
- 插件生命周期钩子（start/stop/init）不做（同 016）；仅 before/after tool_call + should_stop + prepare_next_turn。
- 跨进程/远程 Agent 插件加载不做（同 016/028 边界）。

## Files

- src/plugin/hooks.rs（`LifecycleHooks` + `HookContext` + `HookError` + 组合实现 + 单测）
- src/plugin/agent.rs（`AgentFactory` + `AgentPlugin` + `AgentPluginRegistry` + `AgentPluginError` + 单测）
- src/plugin/mod.rs（`pub mod hooks` + `pub mod agent` + re-export）
- src/lib.rs（re-export `LifecycleHooks`/`AgentFactory`/`AgentPlugin`/`AgentPluginRegistry`）
- src/core/runtime.rs（**仅当** LoopConfig 需新增 `hooks` 桥接字段时，最小改动 + 记录）
- tests/agent_plugin.rs（集成测试：注册/合并 hooks/工厂/桥接）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets -D warnings passes
- [ ] cargo test --all-targets passes
- [ ] cargo fmt --check passes
- [ ] `LifecycleHooks` 默认实现：未覆盖的方法不 panic、不干预主循环（空操作）
- [ ] `AgentPluginRegistry`：register 重复 id → `DuplicateAgentPlugin`；unregister 不存在 → `None`；get/list 正确且 list 字典序
- [ ] `merged_hooks`：多插件 hooks 按 id 字典序链式调用；`before_tool_call` 任一 `Err` 短路（后续插件不被调用，用调用计数断言）；`should_stop_after_turn` 首个 `Some(true)` 即停；无插件贡献 hooks 时返回 `None`
- [ ] `agent_factory(id)` 正确取到工厂；`AgentFactory::build` 产出可用的 `Arc<dyn Agent>`（fake agent 断言 prompt/snapshot 生命周期）
- [ ] 组合 hooks 的调用发生在注册表锁外（fake plugin 在 hook 内重新进入 registry 不产生死锁，对齐 016 r1 教训）
- [ ] `unregister` 语义：已取出的 `Arc<dyn AgentPlugin>` 在 unregister 后仍可调用（Arc 延长生命周期）
- [ ] 桥接：`Arc<dyn LifecycleHooks>` 注入 LoopConfig 后，主循环在工具执行前后实际调用钩子（fake hook 计数断言），且既有闭包钩子行为不回归（既有测试全绿）
- [ ] 产品代码无 `unwrap()`；异步测试用 `tokio::test`；测试用内存 fake plugin/factory/hooks，不硬编码路径、不依赖外部服务
- [ ] 单文件 ≤ 400 行，超则拆子模块并记录

## 修订记录

- v1.0（2026-09，Architect）：初稿。roadmap 候选 4 立项（PM 定序最后，十期第三项）。`LifecycleHooks` trait 化 003 `LoopConfig` 钩子（before/after tool_call + should_stop + prepare_next_turn，默认空实现）；`AgentFactory` + `AgentPlugin` + `AgentPluginRegistry` 支撑自定义 agent 类型注册；零破坏 001/016/003（独立新 trait + 可选桥接，不删旧字段）。`convert_to_llm`/`transform_context` 不开放、动态库加载不做、跨进程不做，均列边界。钩子/上下文具体签名以 003 定稿实际为权威，语义固定（对齐 007 规格「语义固定、形状以实际为准」的处理方式）。
