# Task 003: Tool trait + Runtime 执行引擎

## Background

001 交付了 AgentHandle 骨架（最小内存版，无 LLM）。本任务完成定稿的核心：**单 runtime task 的主循环**，接入 `ModelProvider`（可取消的 provider stream）与 `Tool` trait（顺序/ReadOnly 并行执行），并落实一期行为契约（上下文预算、只重试 provider、CancellationToken 贯穿）。

## Goal

- 定义 `Tool` trait（含参数、执行策略、资源声明）
- 定义 `ModelProvider` trait 与 `AssistantStream`（错误两段式）
- 实现 `run_agent_loop` 主循环（turn 调度 + 工具编排 + steering/followUp + 取消 + 重试）
- 用 **fake provider** 驱动行为测试，不依赖真实 HTTP

## Design Notes

### Tool trait（core/tool.rs）

```rust
// 资源声明：决定并发安全
pub enum ResourceScope {
    ReadOnly,      // 可与其他 ReadOnly 并行
    FileWriter,    // 与 FileWriter 串行（走 file_mutation_queue）
    Exclusive,     // 独占（如 bash，不能与任何写工具并行）
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Option<serde_json::Value>;   // 一期宽松
    fn resource_scope(&self) -> ResourceScope;
    async fn execute(
        &self,
        tool_call_id: &str,
        args: serde_json::Value,        // 已校验
        signal: CancellationToken,
        on_update: Option<&dyn Fn(ToolResult)>,
    ) -> Result<ToolResult, ToolError>;
}

pub struct ToolResult {
    pub content: Vec<ToolResultContent>,
    pub details: Option<serde_json::Value>,
    pub is_error: bool,                 // 或由 Result 表达，实现者裁定
}
```

- 参数校验一期宽松：`serde_json::from_value::<T>()` 工具内部反序列化，失败返回 `ToolError`。
- 工具执行失败**不 throw**，编码进 ToolResult/ToolError 并最终进入 assistant 上下文（Pi 哲学）。

### Provider（core/provider.rs，错误两段式）

```rust
pub type AssistantStream = Pin<Box<dyn Stream<Item = AssistantEvent> + Send + 'static>>;

pub struct ProviderRequest {
    pub model: Model,
    pub context: Context,          // system_prompt + messages + tools
    pub thinking_level: ThinkingLevel,
    pub session_id: Option<String>,
    pub signal: CancellationToken,
}

#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// 建立请求失败 → Err；流建立后一切失败 → 流内 AssistantEvent::Error
    async fn stream(&self, request: ProviderRequest) -> Result<AssistantStream, ProviderError>;
}

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

- 流内 `Error` → 主循环产出终态 `AssistantMessage { stop_reason: Error, error_message }`。
- `AssistantStream` 用 `futures::Stream`；本任务不引入 pin-project-lite。

### Runtime 主循环（core/runtime.rs）

```
loop {
    turn_start
    → stream_assistant_response（消费 AssistantStream，增量收 text/thinking/toolCall）
    → 有 toolCall？
        ├─ 无 → turn_end → steering? → followUp? → 退出
        └─ 有 → prepare（参数校验 + before_tool_call）
              → execute（顺序；仅 ReadOnly 组内并行，Exclusive 独占）
              → after_tool_call → ToolResult 入上下文
              → turn_end → prepare_next_turn → 继续
}
```

```rust
pub struct LoopConfig {
    pub model: Model,
    pub convert_to_llm: fn(Vec<Arc<Message>>) -> Vec<Message>,
    pub transform_context: Option<Box<dyn Fn(Vec<Arc<Message>>, CancellationToken) -> ...>>,
    pub before_tool_call: Option<Box<dyn Fn(...)>>,
    pub after_tool_call: Option<Box<dyn Fn(...)>>,
    pub should_stop_after_turn: Option<Box<dyn Fn(...) -> bool>>,
    pub prepare_next_turn: Option<Box<dyn Fn(...) -> ...>>,
    pub tool_execution: ToolExecutionMode,   // Sequential | ReadOnlyParallel
}
```

### 一期行为契约（必须实现）

| 能力 | 行为 |
|------|------|
| Context window | 每轮请求前按模型 context_window 计算预算（粗估 token）；超限拒绝或保守截断（`transform_context` 钩子，二期摘要） |
| 工具并发 | 默认**顺序**；仅显式 `ReadOnly` 工具可并行；`Exclusive`（bash）不与任何写工具并行 |
| 重试 | **仅重试 provider 请求，不重试工具**；指数退避（0.5s·2^n，上限、可取消、抖动可选） |
| 取消 | 一个 run 一个 `CancellationToken`，贯穿 Provider、Tool、退避等待 |
| 文件修改 | write/edit 串行由 `ResourceScope::FileWriter` + 003 的编排保证（内置工具 004 起） |
| 状态 | agent 内部持有完整 transcript；对外 `AgentSnapshot` 不可变 |

- 003 的 loop 作为 001 `AgentHandle` 的 runtime task 内部行为接入：命令 `Prompt/Continue/Steer/FollowUp` 驱动 loop，事件发往 `broadcast`，snapshot 由 `watch` 更新。

### Fake provider（测试）

- `tests/runtime_loop.rs` 定义 `FakeProvider`：按脚本回放 `AssistantEvent`（如 `ToolCallStart→ToolCallEnd→Done`、或 `TextDelta→Done`、或 `Error`），验证主循环分支：无工具退出、有工具执行、steering/followUp、取消、重试、上下文预算触发截断。

## Files

- src/core/tool.rs
- src/core/provider.rs
- src/core/runtime.rs
- src/core/context.rs（上下文预算/裁剪，`convert_to_llm` 的通用投影放这里）
- src/core/mod.rs
- tests/runtime_loop.rs

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy -D warnings passes
- [ ] cargo test passes
- [ ] cargo fmt --check passes
- [ ] tests/runtime_loop.rs 用 fake provider 覆盖：纯文本一轮结束；toolCall→ToolResult 循环；顺序执行顺序保证；ReadOnly 并行；Exclusive 独占；steering/followUp；abort 后产出 `stop_reason: Aborted`；provider 失败重试（计数可断言）；上下文预算超限触发拒绝/截断
- [ ] 无 `unwrap()`；异步测试 `tokio::test`
- [ ] 单文件超 400 行拆子模块并记录（runtime.rs 大概率需拆，如 runtime/mod.rs + turn.rs + tools.rs）
