# guigu 整体架构设计（定稿 v1.0）

> 状态：已定稿（2025-XX，PM 确认）
> 依据：对 `~/pi`（earendil-works/pi，TypeScript monorepo）的源码分析 + PM 定稿意见

## 0. 设计原则

- 借鉴 Pi 架构思想，**非 1:1 移植**；Rust 原生，追求性能、安全、极简依赖
- 一期只做核心运行时；Session 树 / JSONL 崩溃恢复 / 上下文摘要 / 插件与远程协议均二期，加在稳定接口之外
- 单 crate + 内部模块化（可嵌入，不设 workspace）

## 1. Pi 架构要点（借鉴来源）

```
telemetry(遥测) → agent-core(运行时+session树) → coding-agent(CLI) → tui(UI)
                      ↑
                  pi-ai(多Provider LLM抽象)
```

| 设计 | 说明 | 一期取舍 |
|------|------|----------|
| 消息判别联合 + 内容分段 | 三类消息各带内容段，ToolResult 为顶层消息 | ✅ 采用 |
| 自定义消息扩展 | 非 LLM 消息可注入，convert 投影/过滤 | ✅ 简化内置 |
| StreamFn/Provider 注入 | agent 循环不依赖具体 provider | ✅ 采用 |
| 三层生命周期事件 | agent / turn / message / tool_execution | ✅ 采用 |
| 钩子机制 | before/afterToolCall、shouldStopAfterTurn、prepareNextTurn、steer/followUp | ✅ 采用 |
| 工具抽象 | schema + execute + 串/并行策略 | ✅ 采用 |
| Session 树 + 崩溃恢复 | append-only Entry + lane 记录 + reducer | ⏸ 二期 |
| 错误不 throw | 工具/FS/Shell 返回 Result，错误进 stop_reason | ✅ 天然契合 |
| compaction / branch summary | 上下文压缩与分支汇总 | ⏸ 二期（接口预留） |
| provider 归一化 | 各 provider adapter 做格式转换 | ✅ adapters/ |
| file-mutation-queue | 同文件写串行化防竞态 | ✅ 采用 |
| deferred tools | 按需加载工具 | ⏸ 二期 |

## 2. 目录结构（定稿）

```
/home/fhy/guigu/
├── Cargo.toml
├── src/
│   ├── lib.rs                 # facade 聚合导出
│   ├── core/
│   │   ├── mod.rs
│   │   ├── message.rs         # 消息、内容段、usage、stop reason
│   │   ├── event.rs           # 生命周期与流式事件
│   │   ├── tool.rs            # Tool trait、参数校验、执行策略、资源声明
│   │   ├── provider.rs        # ModelProvider、AssistantStream、请求/响应规范
│   │   ├── runtime.rs         # 单 writer agent loop、命令队列、取消、重试
│   │   ├── context.rs         # 上下文预算、裁剪；二期压缩接口
│   │   └── session.rs         # 一期内存实现；二期 JSONL 树与持久化
│   ├── tools/
│   │   ├── mod.rs
│   │   ├── read.rs  write.rs  edit.rs  bash.rs
│   │   └── file_mutation_queue.rs
│   ├── adapters/
│   │   ├── mod.rs             # 注册表/分发
│   │   ├── openai.rs          # OpenAI 兼容 API 请求/响应转换
│   │   └── anthropic.rs       # Anthropic API 请求/响应转换
│   └── bin/
│       └── main.rs            # 示例 CLI（echo / 交互式）
└── tests/
    ├── message.rs             # 序列化 roundtrip
    ├── runtime_loop.rs        # 主循环行为（fake provider 驱动）
    └── echo_agent.rs          # 最小端到端
```

**职责边界**：
- `convert.rs` 不保留。通用"上下文转换"归 `context.rs`；OpenAI/Anthropic 格式转换归 `adapters/`，不做模糊公共模块。
- `core/` 纯领域：消息/事件/trait/主循环，不依赖 HTTP。
- `adapters/` 依赖 `reqwest`（feature-gated）。

## 3. 核心抽象设计

### 3.1 消息模型（core/message.rs）

**不使用过宽的 Content 枚举**。每类消息独立内容段，ToolResult 是顶层消息，不嵌入 Content，保证拓扑合法：

```
User -> Assistant(ToolCall) -> ToolResult -> Assistant
```

```rust
pub enum Message {
    User(UserMessage),
    Assistant(AssistantMessage),
    ToolResult(ToolResultMessage),
}

pub enum UserContent {
    Text(String),
    Image(ImageContent),
}

pub enum AssistantContent {
    Text(String),
    Thinking(String),
    ToolCall(ToolCall),
}

pub enum ToolResultContent {
    Text(String),
    Image(ImageContent),
}

pub struct UserMessage      { pub content: Vec<UserContent>,      pub timestamp: u64 }
pub struct AssistantMessage {
    pub content: Vec<AssistantContent>,
    pub model: Option<ModelId>,
    pub usage: Option<Usage>,
    pub stop_reason: Option<StopReason>,  // "completed"|"length"|"error"|"aborted"|...
    pub error_message: Option<String>,
    pub timestamp: u64,
}
pub struct ToolResultMessage {
    pub tool_call_id: String,
    pub tool_name: String,
    pub is_error: bool,
    pub content: Vec<ToolResultContent>,
    pub details: Option<serde_json::Value>,
    pub timestamp: u64,
}
```

### 3.2 事件模型（core/event.rs）

```rust
pub enum AgentEvent {
    AgentStart,
    AgentEnd { messages: Vec<Arc<Message>> },
    TurnStart,
    TurnEnd { message: Arc<AssistantMessage>, tool_results: Vec<ToolResultMessage> },
    MessageStart { message: Arc<Message> },
    MessageUpdate { message: Arc<Message>, assistant_event: AssistantEvent },
    MessageEnd { message: Arc<Message> },
    ToolExecutionStart { tool_call_id, tool_name, args },
    ToolExecutionUpdate { tool_call_id, tool_name, args, partial: ToolResult },
    ToolExecutionEnd { tool_call_id, tool_name, result, is_error },
}
```

### 3.3 并发模型（核心决策：单 writer）

**不采用 `Arc<RwLock<AgentState>>` 作为核心设计**。理想模型是**单 writer + 唯一 Runtime task**：

```
AgentHandle                      （对外句柄，Clone）
   │  mpsc::Sender<Command>      （prompt/continue/steer/followUp/abort/reset）
   ▼
唯一 Runtime task                 （顺序消费命令，唯一写者）
   │  ├─ 顺序修改 transcript / session
   │  ├─ watch::Sender<AgentSnapshot>    权威最新状态（UI 随时读取）
   │  └─ broadcast::Sender<AgentEvent>   瞬态增量事件（渲染）
   ▼
AgentSnapshot / AgentEvent       （订阅方）
```

- `watch`：UI 随时拿最新状态，**权威**。
- `broadcast`：只传增量事件（瞬态渲染）。订阅者 lag 时**重读 snapshot**，不把 broadcast 当审计或持久化通道。
- 主循环与工具执行期间**不持锁跨 await**。
- `AgentSnapshot` 对外不可变；agent 内部持有完整 transcript。

### 3.4 Tool trait（core/tool.rs）

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Option<serde_json::Value>;   // 一期宽松，二期 schemars
    /// 资源声明：一期用于判定并发安全性
    fn resource_scope(&self) -> ResourceScope;            // ReadOnly | FileWriter | Exclusive
    async fn execute(
        &self,
        tool_call_id: &str,
        args: serde_json::Value,
        signal: CancellationToken,
        on_update: Option<&dyn Fn(ToolResult)>,
    ) -> Result<ToolResult, ToolError>;
}
```

参数一期宽松：`serde_json::from_value::<T>()` 工具内部反序列化，失败返回工具错误。

### 3.5 ModelProvider / AssistantStream（core/provider.rs）

```rust
pub type AssistantStream =
    Pin<Box<dyn Stream<Item = AssistantEvent> + Send + 'static>>;

#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// 建立请求失败 → 外层 Err；流建立后的一切失败 → 流内 AssistantEvent::Error
    async fn stream(&self, request: ProviderRequest)
        -> Result<AssistantStream, ProviderError>;
}
```

**错误两段式（比"所有错误都不返回 Result"更可判定）**：
- **建立请求失败**（网络不通、认证失败、参数非法）→ 外层 `Result::Err`
- **流内失败**（流建立后的网络断、协议错、模型错）→ `AssistantEvent::Error`，主循环据此产出终态 `AssistantMessage { stop_reason: Error }`

```rust
pub enum AssistantEvent {
    TextDelta { text: String },
    ThinkingDelta { thinking: String },
    ToolCallStart { id: String, name: String, arguments: String },
    ToolCallDelta { id: String, arguments_delta: String },
    ToolCallEnd { id: String },
    Done { message: AssistantMessage },
    Error { message: String, aborted: bool },
}
```

### 3.6 Runtime 主循环（core/runtime.rs）

```
loop {
    turn_start
    → stream_assistant_response（收集 text/thinking/toolCall）
    → 有 toolCall？
        ├─ 无 → turn_end → steering? → followUp? → 退出
        └─ 有 → prepare（参数校验 + before_tool_call）
              → execute（sequential / 仅 ReadOnly 并行，after_tool_call）
              → ToolResult 入上下文 → turn_end → prepare_next_turn → 继续
}
```

`LoopConfig` 钩子：`convert_to_llm` / `transform_context` / `before_tool_call` / `after_tool_call` / `should_stop_after_turn` / `prepare_next_turn` / `tool_execution` 策略。

### 3.7 上下文（core/context.rs）

- 每轮请求前**计算 token 预算**（估算 + 模型 context_window）
- 超限：**先拒绝或保守截断**；二期加入摘要压缩（`Compactor` trait 预留接口）
- 负责通用上下文转换（非 LLM 消息投影/过滤）

### 3.8 会话（core/session.rs）

- **一期**：`InMemorySessionStorage`（append-only Entry 内存实现）
- **二期**：Session 树 / 分支 fork / JSONL 文件后端 / 崩溃恢复（reducer）
- 预留 `SessionStorage` trait 接口，二期在稳定接口之外扩展

## 4. 一期行为契约（必须补齐）

| 能力 | 一期行为 |
|------|----------|
| Context window | 每轮请求前计算预算；超限先拒绝或保守截断；二期加摘要压缩 |
| 工具并发 | 默认**顺序**执行；仅显式 ReadOnly 工具可并行 |
| 重试 | **仅重试 provider 请求，不重试工具**；指数退避、上限、可取消 |
| 取消 | 一个 run 一个 `CancellationToken`，传给 Provider、Tool、退避等待和子进程 |
| 文件修改 | write/edit **串行**；bash 默认视为独占文件系统，不能与写工具并行 |
| 状态 | agent 内部持有完整 transcript；对外给不可变 `AgentSnapshot` |

## 5. 技术选型与依赖

| 用途 | 依赖 | 说明 |
|------|------|------|
| 异步运行时 | `tokio` | rt-multi-thread / macros / sync / time / io-util |
| 序列化 | `serde` + `serde_json` | 消息、事件、会话 |
| 错误 | `thiserror` | 按 conventions |
| 日志 | `tracing` | 替代 pi-telemetry |
| 取消 | `tokio-util` | CancellationToken |
| 流 | `futures` | Stream/BoxStream；极致精简可用 futures-core + futures-util |
| HTTP | `reqwest` | **仅 `providers-http` feature 下**，核心库不依赖 |
| 异步 trait | `async-trait` | **取决于一期是否支持动态工具注册**：静态工具可用原生 async fn in trait（edition 2024）；动态扩展则显式 boxed future 或保留 |

**刻意不引入**：`pin-project-lite`（仅手写 Stream/Future 状态机时再加）、schemars/jsonschema（二期）、clap（二期可选）、workspace 多 crate。

## 6. 一期范围与里程碑

**一期交付**：单 runtime task、统一消息模型、可取消的 provider stream、顺序工具执行、内存 session。

| 任务 | 内容 | 涉及文件 |
|------|------|----------|
| 001 | Agent trait + 生命周期（AgentHandle/snapshot/订阅/命令） | core/agent.rs, core/event.rs |
| 002 | Message/Event 数据结构（含序列化 roundtrip 测试） | core/message.rs, core/event.rs |
| 003 | Runtime 执行引擎（单 writer loop + LoopConfig + fake provider） | core/runtime.rs, core/provider.rs |
| 004 | 最小 Echo Agent（端到端） | core/tool.rs, src/bin/main.rs, tests/ |

**二期（稳定接口之外）**：Session 树 / JSONL 崩溃恢复 / 上下文摘要 / 插件与远程协议 / deferred tools。

一期验收：`cargo check` / `cargo clippy -D warnings` / `cargo test` / `cargo fmt --check` 四门全绿。
