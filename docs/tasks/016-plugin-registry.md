# Task 016: 插件机制（Plugin Registry + 异步工具实例化）

## Background

001–015 已交付完整运行时：消息/事件、AgentHandle 生命周期、Runtime 主循环、内置工具（echo/read/write/edit/bash）、adapters、上下文压缩、Session 树 + 崩溃恢复、远程协议、deferred tools、多 lane session、Agent Server、ACP、CLI。

architecture §7.2 明确「插件机制延后」，并指出 **011 deferred tools（schema 与执行体分离）为未来插件注册表前置**。011 已落地 `DeferredToolSpec`（owned schema 元数据）与 `DeferredTool`（同步、infallible 的 `OnceLock` 惰性工厂），但 011 规格 v1.0 修订记录同时声明：**「真正需 async 实例化（如远程加载）属后续『插件』任务，不在本任务」**。

本任务补上这一缺口：在 `DeferredToolSpec` 之上，引入**可失败、可异步实例化**的工具插件抽象 + 一个进程内**插件注册表**，把工具 schema 常驻、执行体惰性加载的机制从「单工具」扩展到「多插件、多工具」的组织面。零破坏：产出物仍是合法 `Tool`，仍入 `Vec<Arc<dyn Tool>>`。

## Goal

- 定义 `Plugin` trait：一个插件声明**唯一 id** + **贡献的工具 schema 集合** + **异步可失败的执行体实例化**。
- 定义 `PluginTool`：实现 `Tool` 的**异步惰性包装器**——schema 常驻、执行体首次 `execute` 才异步构建并缓存，失败**不缓存、可重试**。
- 定义 `PluginRegistry`：进程内注册表，register/unregister/get/list + 组装全插件工具为 `Vec<Arc<dyn Tool>>`（可直接喂给 `AgentRuntime.tools`）。
- **零破坏**：不改 `Tool` trait、不改 `AgentRuntime.tools` 契约、不改 003 主循环。

## Design Notes

### 契约复用（勿改）

- `Tool` trait 完整签名（003 定稿，core/tool.rs）：`execute(&self, tool_call_id: &str, args: serde_json::Value, signal: CancellationToken, on_update: Option<&dyn Fn(ToolResult)>) -> Result<ToolResult, ToolError>`。
- `ResourceScope::{ ReadOnly, FileWriter, Exclusive }`、`ToolResult`、`ToolError` 定于 core/tool.rs（003 定稿）。
- `DeferredToolSpec`（011 定稿，src/tools/deferred.rs）：`{ name, description, parameters, resource_scope }`，owned，`Clone`。
- 工具经 `AgentRuntime { tools: Vec<Arc<dyn Tool>> }` 注册（003 定稿），双参 spawn（004 核验）。

### Plugin trait（src/plugin/mod.rs）

```rust
#[async_trait]
pub trait Plugin: Send + Sync {
    /// 稳定唯一标识（注册表主键）。
    fn id(&self) -> &str;

    /// 贡献的工具 schema 元数据。注册/组装时即用，**绝不触发实例化**。
    fn tools(&self) -> Vec<DeferredToolSpec>;

    /// 异步实例化某工具执行体（可失败）。惰性：仅首次 `execute` 时被调用。
    async fn instantiate(&self, name: &str) -> Result<Arc<dyn Tool>, PluginError>;
}
```

- `async_trait`（动态分发 `Arc<dyn Plugin>` 需要 boxed future）。
- `tools()` 同步返回 owned schema；`instantiate(name)` 的 `name` 必须命中 `tools()` 中某个 spec 的 `name`，否则 `PluginError::ToolNotDeclared`。

### PluginTool（src/plugin/tool.rs）

```rust
/// 异步惰性工具：schema 常驻，执行体首次 execute 时异步构建并缓存；失败不缓存、可重试。
pub struct PluginTool {
    spec: DeferredToolSpec,
    plugin: Arc<dyn Plugin>,
    name: String,
    inner: tokio::sync::OnceCell<Arc<dyn Tool>>,
}

impl PluginTool {
    /// spec.name 与 name 一致；plugin 与 spec 由 PluginRegistry::tools() 装配时注入。
    pub fn new(plugin: Arc<dyn Plugin>, spec: DeferredToolSpec) -> Self;
}

impl Tool for PluginTool {
    fn name(&self) -> &str { &self.spec.name }
    fn description(&self) -> &str { &self.spec.description }
    fn parameters(&self) -> Option<serde_json::Value> { self.spec.parameters.clone() }
    fn resource_scope(&self) -> ResourceScope { self.spec.resource_scope }

    async fn execute(
        &self,
        tool_call_id: &str,
        args: serde_json::Value,
        signal: CancellationToken,
        on_update: Option<&dyn Fn(ToolResult)>,
    ) -> Result<ToolResult, ToolError> {
        let tool = self.inner
            .get_or_try_init(|| async {
                self.plugin.instantiate(&self.name).await
                    .map_err(plugin_to_tool_error)
            })
            .await
            .map_err(plugin_to_tool_error)?;
        tool.execute(tool_call_id, args, signal, on_update).await
    }
}
```

设计要点（务必落实）：

1. **schema 常驻、执行体惰性**：`name/description/parameters/resource_scope` 只读 `spec`，**绝不触发** `instantiate`；`execute` 首次才异步构建。
2. **异步只初始化一次**：用 `tokio::sync::OnceCell::get_or_try_init`——并发 `execute` 共享同一次初始化 future，工厂只跑一次；内部是 async 协调，**不跨 await 持 std 锁**，不阻塞 runtime。
3. **失败不缓存、可重试**：`get_or_try_init` 语义为「失败则不落 cell，下次调用重试」。这是与 011 `DeferredTool`（infallible，失败即 panic）的关键差异：插件实例化可能因 IO/远程暂时失败，允许下一次 `execute` 重试。
4. **错误映射**：`instantiate` 的 `PluginError` 经 `plugin_to_tool_error` 映射为 `ToolError`（因 `Tool::execute` 返回 `ToolError`）。映射语义为「执行体实例化失败」：**若 003 的 `ToolError` 已存在可承载字符串/错误消息的变体（如 `Error(String)` / `Execution(String)`），复用之；否则 Developer 补一个变体承载 `PluginError` 的 `Display`，并在修订记录 + commit message 说明**。
5. **`on_update`/`signal`/`args`/`tool_call_id` 原样透传**给内层工具，不捕获不复制。
6. **线程安全**：`Tool: Send + Sync`（003）→ `Arc<dyn Plugin>: Send + Sync`、`tokio::sync::OnceCell<Arc<dyn Tool>>: Send + Sync` → `PluginTool: Send + Sync`，可入 `Vec<Arc<dyn Tool>>`。

### PluginRegistry（src/plugin/mod.rs）

```rust
pub struct PluginRegistry {
    plugins: std::sync::RwLock<HashMap<String, Arc<dyn Plugin>>>,
}

impl PluginRegistry {
    pub fn new() -> Self;

    /// 注册插件；id 已存在 → PluginError::DuplicatePlugin。
    pub fn register(&self, plugin: Arc<dyn Plugin>) -> Result<(), PluginError>;

    /// 卸载插件，返回被移除的插件（不存在 → None）。
    pub fn unregister(&self, id: &str) -> Option<Arc<dyn Plugin>>;

    pub fn get(&self, id: &str) -> Option<Arc<dyn Plugin>>;

    /// 已注册插件 id 列表，**按 id 字典序稳定排序**。
    pub fn list(&self) -> Vec<String>;

    /// 组装所有插件的所有工具为 `Vec<Arc<dyn Tool>>`（每个 spec 包一个 `PluginTool`）。
    /// 顺序**确定**：插件按 id 字典序，插件内按 `tools()` 声明顺序。
    pub fn tools(&self) -> Vec<Arc<dyn Tool>>;
}
```

设计要点：

1. **用 `std::sync::RwLock`，非 tokio Mutex**：`register/unregister/get/list/tools` 全同步、无 await，短临界区，用 std 锁更轻且避免误跨 await。`instantiate` 的 async 逻辑在 `PluginTool::execute` 内部，**不经过注册表锁**。
2. **确定性顺序**：`tools()` 按插件 id 字典序 + 声明序组装，保证 system prompt 组装稳定（HashMap 迭代序不确定，必须显式排序）。
3. **unregister 语义**：只阻止新注册/新组装引用该插件；已分发出去的 `PluginTool` 持有 `Arc<dyn Plugin>`，仍可正常实例化执行（Arc 延长生命周期）。此为**期望语义**，在 doc comment 写明。
4. **重复工具名**：同一插件内两个 spec 同名，或跨插件同名工具，`tools()` 不拦截（交给上层按需处理）；本任务不做全局唯一性校验（属上层装配策略，声明为边界）。

### PluginError（src/plugin/mod.rs）

```rust
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("plugin already registered: {0}")]   DuplicatePlugin(String),
    #[error("plugin not found: {0}")]            PluginNotFound(String),
    #[error("tool `{0}` not declared by plugin")] ToolNotDeclared(String),
    #[error("plugin `{plugin}` failed to instantiate tool `{tool}`: {source}")]
    Instantiation {
        plugin: String,
        tool: String,
        #[source] source: Box<dyn std::error::Error + Send + Sync>,
    },
}
```

- `Instantiation` 携带 `#[source]`，便于上层（若未来有）链式诊断；经 `plugin_to_tool_error` 映射进 `ToolError` 时取 `to_string()`。

### 边界声明（明确不做）

- **动态库加载（libloading / .so / .dylib）**：不做。「插件」= 进程内 `Arc<dyn Plugin>` trait 对象，由嵌入方注入；真实 dlopen 涉及 unsafe/ABI，属后续。
- **Agent 插件**：本任务只做**工具插件**（补齐 011 声明的「async 工具实例化」缺口）。插件贡献 Agent trait 对象不在范围。
- **插件依赖解析 / 生命周期钩子（start/stop）**：不做；插件只暴露 `id/tools/instantiate` 三件事。
- **全局工具名唯一性校验 / 冲突仲裁**：不做（见上，属上层装配策略）。
- **跨进程插件共享 / 远程加载**：不做（同 010/013 边界）。

## Files

- src/plugin/mod.rs（`Plugin` trait + `PluginError` + `PluginRegistry` + 单测）
- src/plugin/tool.rs（`PluginTool` + 单测）
- src/lib.rs（登记 `pub mod plugin` + re-export `Plugin`/`PluginError`/`PluginRegistry`/`PluginTool`）
- src/core/tool.rs（**仅当** `ToolError` 缺字符串承载变体时补一个，不动其他）
- tests/plugin.rs（集成测试，走完整 Tool trait 契约）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets -D warnings passes
- [ ] cargo test --all-targets passes
- [ ] cargo fmt --check passes
- [ ] **惰性 + 异步只初始化一次**：构造 `PluginTool` 后工厂调用计数为 0；调用 schema 四方法后仍为 0；并发 `execute`（`join_all`）后计数为 1；再次 `execute` 仍为 1（用 `Arc<AtomicUsize>` 计数的 fake plugin 断言）
- [ ] **失败可重试**：fake plugin 首次 `instantiate` 返回 `Err`，第二次返回 `Ok`；首次 `execute` 返回 `ToolError`，第二次 `execute` 成功，且工厂计数为 2（证明失败不缓存）
- [ ] **schema 转发**：`PluginTool` 的 schema 四方法与 `DeferredToolSpec` 完全一致
- [ ] **execute 透传**：实例化成功后 `tool_call_id`/`args`/`signal`/`on_update` 原样转发，返回结果与内层工具一致
- [ ] **注册表**：register 重复 id → `DuplicatePlugin`；unregister 不存在 → `None`；get/list 正确；list 按 id 字典序
- [ ] **tools() 组装**：多插件、插件多工具场景，返回的 `Vec<Arc<dyn Tool>>` 顺序 = 插件 id 字典序 × 声明序，且每个元素为 `PluginTool`（schema 可读、未触发实例化）
- [ ] **unregister 语义**：组装出的 `PluginTool` 在 unregister 后仍可 `execute` 成功（Arc 延长生命周期）
- [ ] **错误路径**：`instantiate` 对未声明工具名 → `ToolNotDeclared`（或等价语义），映射为 `ToolError`
- [ ] 产品代码无 `unwrap()`；异步测试用 `tokio::test` 真实执行；测试用内存 fake plugin，不硬编码路径、不依赖外部服务
- [ ] 单文件 ≤ 400 行，超则拆子模块并记录

## 修订记录

- v1.0（2026-09-04，Architect）：初稿。四期开篇，补齐 architecture §7.2 与 011 修订记录声明的「插件」缺口——`Plugin` trait（id + owned schema 集合 + async 可失败实例化）+ `PluginTool`（`tokio::sync::OnceCell::get_or_try_init` 异步惰性、失败不缓存可重试）+ `PluginRegistry`（std RwLock 注册表、确定性工具组装）；零破坏（产出物是合法 Tool，入 `Vec<Arc<dyn Tool>>`）；动态库加载、Agent 插件、插件生命周期钩子、跨进程/远程加载均明确排除。
