# Task 011: 工具惰性加载（Deferred Tools）

## Background

001–010 已交付完整运行时闭环：消息/事件、AgentHandle 生命周期、Runtime 主循环、内置工具（echo/read/write/edit/bash）、adapters（OpenAI/Anthropic）、上下文压缩、Session 树 + 崩溃恢复、远程协议。

当前工具的注册契约是 003 定稿的 `AgentRuntime { tools: Vec<Arc<dyn Tool>> }`——**启动时即要求全部工具实例化完毕**。这对「重资源工具」不友好：一个工具的执行体可能涉及打开文件、初始化子进程、加载大 schema 等开销，但 agent 单次 run 未必用得到；而 `Tool` trait 的 `name()/description()/parameters()/resource_scope()`（schema 元数据）才是组装 system prompt / 注册表真正需要常驻的部分。

architecture 1 表中 deferred tools（按需加载工具）明确列为「⏸ 二期」，且为 architecture 6 二期「插件」项的合理前置——先把「schema 元数据」与「执行体」分离并支持惰性实例化，后续插件/工具注册表才能在其上扩展。本任务即补上这一缺口。

## Goal

- 定义 `DeferredToolSpec`：工具 schema 元数据的 **owned** 表示（`name` / `description` / `parameters` / `resource_scope`），与执行体分离
- 实现 `DeferredTool`：实现 `Tool` trait 的**惰性包装器**——schema 常驻，执行体首次 `execute` 时经工厂构建并缓存（进程内仅构建一次）
- 提供便捷构造：`DeferredTool::new`（spec + 工厂）、`DeferredTool::lazy`（自动 Box 工厂）、`DeferredTool::ready`（已实例化工具直接包装）
- **零破坏**：`DeferredTool` 本身就是一个合法 `Tool` 实现，仍放进 `Vec<Arc<dyn Tool>>`，不改 `AgentRuntime.tools` 契约、不改 003 主循环

## Design Notes

### 契约复用（以既有定稿为准，勿改签名）

- `Tool` trait 完整签名（003 定稿，core/tool.rs，勿改）：
  `execute(&self, tool_call_id: &str, args: serde_json::Value, signal: CancellationToken, on_update: Option<&(dyn Fn(ToolResult) + Send + Sync)>) -> Result<ToolResult, ToolError>`
- `ResourceScope::{ ReadOnly, FileWriter, Exclusive }`（003 定稿，core/tool.rs）
- `ToolResult` / `ToolError` 定于 core/tool.rs（003 定稿）
- 工具落 `src/tools/`（004 已建 mod.rs），经 `AgentRuntime { tools: Vec<Arc<dyn Tool>> }` 注册（003 定稿）

### DeferredToolSpec（src/tools/deferred.rs）

> **命名说明（v1.1）**：新类型命名为 `DeferredToolSpec` 而非 `ToolSpec`，避免与既有稳定类型 `core::provider::ToolSpec`（003/007 定稿，LLM 请求 wire 格式 `{ name, description, parameters }`）撞名。两者都经 `lib.rs` glob 重导出到顶层，若同名则 `guigu::ToolSpec` 产生歧义。`DeferredToolSpec` 语义更准确（DeferredTool 消费的 owned schema 元数据），且零破坏——不动 provider 侧任何契约。

```rust
/// 工具 schema 元数据的 owned 表示，与执行体分离，供 defer 场景常驻。
#[derive(Debug, Clone)]
pub struct DeferredToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Option<serde_json::Value>,
    pub resource_scope: ResourceScope,
}

impl DeferredToolSpec {
    /// 从已实例化工具抽取 schema（&str → String，parameters 直接 move）。
    pub fn from_tool(tool: &dyn Tool) -> Self {
        Self {
            name: tool.name().to_string(),
            description: tool.description().to_string(),
            parameters: tool.parameters(),            // 003 定稿为 owned Value
            resource_scope: tool.resource_scope(),     // 需 ResourceScope: Clone
        }
    }
}
```

- `from_tool` 依赖 `ResourceScope` 可拷贝：**若 003 的 `ResourceScope` 未 `derive(Clone)`，Developer 补 `#[derive(Debug, Clone, Copy, PartialEq, Eq)]`（纯 derive，不变体语义，改动记录在规格修订记录与 commit message）。**
- `DeferredToolSpec` 全字段公开，供外部显式构造（不经 `from_tool`）。

### DeferredTool（src/tools/deferred.rs）

```rust
/// 惰性工具：schema 常驻，执行体首次 execute 时经工厂构建并缓存（进程内仅一次）。
pub struct DeferredTool {
    spec: DeferredToolSpec,
    factory: Box<dyn Fn() -> Arc<dyn Tool> + Send + Sync>,
    inner: std::sync::OnceLock<Arc<dyn Tool>>,
}

impl DeferredTool {
    /// spec + 工厂闭包。工厂须 infallible（返回 Arc<dyn Tool>，不返回 Result）。
    pub fn new(spec: DeferredToolSpec, factory: Box<dyn Fn() -> Arc<dyn Tool> + Send + Sync>) -> Self;

    /// 便捷构造：工厂闭包自动 Box。
    pub fn lazy(spec: DeferredToolSpec, factory: impl Fn() -> Arc<dyn Tool> + Send + Sync + 'static) -> Self;

    /// 已实例化工具直接包装（内部工厂返回自身，不产生额外构建）。
    pub fn ready(tool: Arc<dyn Tool>) -> Self;
}

impl Tool for DeferredTool {
    fn name(&self) -> &str { &self.spec.name }
    fn description(&self) -> &str { &self.spec.description }
    fn parameters(&self) -> Option<serde_json::Value> { self.spec.parameters.clone() }
    fn resource_scope(&self) -> ResourceScope { self.spec.resource_scope }

    async fn execute(
        &self,
        tool_call_id: &str,
        args: serde_json::Value,
        signal: CancellationToken,
        on_update: Option<&(dyn Fn(ToolResult) + Send + Sync)>,
    ) -> Result<ToolResult, ToolError> {
        let tool = self.inner.get_or_init(|| (self.factory)());
        tool.execute(tool_call_id, args, signal, on_update).await
    }
}
```

### 设计要点（务必落实）

1. **schema 常驻、执行体惰性**：`name/description/parameters/resource_scope` 四个方法只读 `spec`，**绝不触发工厂**；`execute` 首次调用才 `get_or_init` 构建执行体。
2. **只构建一次**：`std::sync::OnceLock` 保证多线程/多 task 并发 `execute` 下工厂仅调用一次（`get_or_init` 内部加锁，闭包同步执行完毕即释放，**不跨 await 点**，符合「不持锁跨 await」原则）。
3. **工厂同步 + infallible**：工厂签名 `Fn() -> Arc<dyn Tool>`（非 async、非 Result）。一期假设工具实例化为同步轻量操作；真正需 async 实例化（如远程加载）属后续「插件」任务，不在本任务。
4. **`on_update` 透传**：`Option<&(dyn Fn(ToolResult) + Send + Sync)>` 为借用，`DeferredTool::execute` 原样转发给内层工具，不捕获、不复制。
5. **参数转发**：`tool_call_id` / `args` / `signal` 原样转发；`tool` 为 `&Arc<dyn Tool>`，经自动解引用以 `&dyn Tool` 调用 `execute`。
6. **`ready` 的 factory**：构造时用 `DeferredToolSpec::from_tool(&*tool)` 抽取 schema，factory 捕获 `tool` 并返回其 clone（`Arc::clone`）；因 schema 已抽取，`ready` 的工厂理论上不会被调用（除非内部 `inner` 逻辑需要，实现者保证不会双重实例化）。
7. **线程安全边界**：`Tool: Send + Sync`（003 定稿）→ `Arc<dyn Tool>: Send + Sync` → `OnceLock<Arc<dyn Tool>>: Send + Sync`；工厂 `Box<dyn Fn() -> Arc<dyn Tool> + Send + Sync>` 使 `DeferredTool: Send + Sync`，可安全放入 `Vec<Arc<dyn Tool>>` 供单 writer runtime 或跨 task 共享。

### 使用示例（Design Notes 说明，非交付代码）

```rust
// 重资源工具惰性注册：启动时仅持有 schema，执行体延后到真正被调用时构建
let spec = DeferredToolSpec { name: "bash".into(), description: "...".into(),
                      parameters: None, resource_scope: ResourceScope::Exclusive };
let tool: Arc<dyn Tool> = Arc::new(DeferredTool::lazy(spec, || Arc::new(BashTool::new(queue.clone()))));
// 放进 AgentRuntime { tools: vec![tool], .. } 即可，runtime 契约零改动
```

## Files

- src/tools/deferred.rs（`DeferredToolSpec` + `DeferredTool` + 单元测试）
- src/tools/mod.rs（`pub mod deferred` + re-export `DeferredToolSpec` / `DeferredTool`）
- src/lib.rs（核对 004 的 `pub use tools::*` 是否覆盖新增项，未覆盖则补 re-export）
- src/core/tool.rs（仅当 `ResourceScope` 缺 `Clone` 时补 derive，不动其他）
- tests/deferred.rs（集成测试，走完整 Tool trait 契约 + 并发只构建一次）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets -D warnings passes
- [ ] cargo test --all-targets passes
- [ ] cargo fmt --check passes
- [ ] **惰性性**：构造 `DeferredTool` 后工厂计数为 0；调用 `name/description/parameters/resource_scope` 后仍为 0；首次 `execute` 后为 1；再次 `execute` 后仍为 1（缓存复用，用 `Arc<AtomicUsize>` 计数的工厂断言）
- [ ] **schema 转发**：`DeferredTool` 的 `name/description/parameters/resource_scope` 与 `DeferredToolSpec` 完全一致（`parameters` 为 `spec.parameters.clone()`）
- [ ] **execute 透传**：工厂返回的记录型 fake tool，`DeferredTool::execute` 原样转发 `tool_call_id`/`args`/`signal`，返回结果与内层工具一致
- [ ] **并发只构建一次**：`tokio::test` + `join_all` 多 task 并发 `execute`，工厂计数仍为 1
- [ ] **`ready`**：`DeferredTool::ready(Arc::new(EchoTool))` 的 `name()=="echo"` 且可直接 `execute`
- [ ] 工厂 panic 不测试（声明为 infallible 契约）；`DeferredToolSpec::from_tool` 从 `&dyn Tool` 正确抽取四项
- [ ] 产品代码无 `unwrap()`；异步测试用 `tokio::test` 真实执行；测试用 `std::env::temp_dir()` 或内存工具，不硬编码路径
- [ ] 单文件 ≤ 400 行，超则拆子模块并记录

## 修订记录

- v1.0（2026-09-01，Architect）：初稿。补 architecture 二期的 deferred tools 缺口——「schema 元数据（ToolSpec，owned）」与「执行体」分离，`DeferredTool` 用 `std::sync::OnceLock` 惰性构建执行体且进程内只构建一次；零破坏（DeferredTool 本身是合法 Tool，仍入 `Vec<Arc<dyn Tool>>`，不改 003 主循环与注册契约）；工厂为同步 + infallible，async 实例化属后续「插件」任务；并发只构建一次由 OnceLock 保证且不跨 await 持锁。
- v1.1（2026-09-01，Architect，依据 Developer 架构审查）：新类型 `ToolSpec` 改名 `DeferredToolSpec`，消除与既有 `core::provider::ToolSpec`（003/007 定稿 wire 格式）顶层 glob 重导出的撞名歧义。方案 A（改名，不动 provider 侧）；零破坏，语义更准确。签名与结构不变，仅命名避撞。同时修正伪代码 `on_update` 为 `Option<&(dyn Fn(ToolResult) + Send + Sync)>`（对齐 003 真实签名）。
