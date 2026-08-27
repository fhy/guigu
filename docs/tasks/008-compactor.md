# Task 008: 上下文摘要压缩 Compactor

## Background

003 已落地一期行为契约「每轮请求前按模型 context_window 计算 token 预算，超限拒绝或保守截断」，且 003 规格明确「二期摘要」由 `transform_context` / `context.rs` 承接。当 agent 长对话的 transcript 持续膨胀，超预算后一期只能**拒绝或保守截断（丢消息）**，会丢失关键上下文。

本任务落地二期摘要压缩：超预算时把**较早的消息压缩为一条摘要**，用摘要替换旧消息，在预算内保留语义，避免丢信息。007 已交付真实 `ModelProvider` 实现（OpenAI/Anthropic），008 的 `LlmCompactor` 复用该 trait 调用真实 LLM 生成摘要——**代码只依赖 `ModelProvider` trait，不依赖具体 adapter**；测试用 fake provider，不依赖网络。

## Goal

- 新增 `Compactor` trait + `LlmCompactor` 实现（持有 `ModelProvider`，用 LLM 生成摘要）
- 编排逻辑：每轮 provider 请求前做 token 预算检查，超限时压缩旧消息（保留最近 K 条），摘要注入 transcript
- 压缩失败降级为保守截断（一期行为），不阻断 agent 运行
- `LlmCompactor` 与编排逻辑均可**脱离网络单测**（fake provider 驱动）

## Design Notes

### 契约复用（以 003 定稿为准，勿改签名）

`ModelProvider` / `AssistantStream` / `AssistantEvent` / `ProviderRequest` / `Message` 均已定稿，008 **不修改其签名**，只消费：

```rust
#[async_trait]
pub trait ModelProvider: Send + Sync {
    async fn stream(&self, request: ProviderRequest) -> Result<AssistantStream, ProviderError>;
}

pub enum AssistantEvent {
    TextDelta { text: String },
    ThinkingDelta { thinking: String },
    ToolCallStart { .. }, ToolCallDelta { .. }, ToolCallEnd { .. },
    Done { message: AssistantMessage },
    Error { message: String, aborted: bool },
}
```

- `ProviderRequest` 含 `model` / `context`（system_prompt + messages + tools）/ `thinking_level` / `session_id` / `signal`（形状以 003 实际实现为准）。
- `Message` 枚举：`User` / `Assistant` / `ToolResult`（002 定稿，`Arc<Message>` 在 transcript 中共享）。

### Compactor trait（core/compactor.rs，职责单一：生成摘要）

```rust
pub struct CompactionRequest {
    pub messages: Vec<Arc<Message>>,   // 待摘要的旧消息（由调用方选定，非全量 transcript）
    pub signal: CancellationToken,
}

pub struct CompactionResult {
    pub summary: String,               // 摘要文本
}

#[derive(Debug, thiserror::Error)]
pub enum CompactionError {
    #[error("summary provider error: {0}")]
    Provider(#[from] ProviderError),   // 摘要 LLM 调用失败（透传 003 ProviderError）
    #[error("compaction cancelled")]
    Cancelled,                          // signal 取消
    #[error("no messages to compact")]
    EmptyInput,                         // 待压缩消息为空
    #[error("empty summary produced")]
    EmptySummary,                       // 流结束未累积任何摘要文本（视为错误，触发降级）
}

#[async_trait]
pub trait Compactor: Send + Sync {
    async fn compact(&self, req: CompactionRequest) -> Result<CompactionResult, CompactionError>;
}
```

- **职责边界**：`Compactor` 只负责「给一批消息 → 产出一条摘要」。哪些消息该压缩、保留多少、如何替换 transcript，属于 context/runtime 的编排职责（见下），不放入 Compactor。保证 Compactor 可独立单测。

### LlmCompactor（core/compactor.rs）

```rust
pub struct LlmCompactor {
    provider: Arc<dyn ModelProvider>,
    model: Model,                 // 摘要所用模型标识
    summary_prompt: String,       // 摘要 system prompt
}

impl LlmCompactor {
    pub fn new(provider: Arc<dyn ModelProvider>, model: Model, summary_prompt: impl Into<String>) -> Self;
}

#[async_trait]
impl Compactor for LlmCompactor {
    async fn compact(&self, req: CompactionRequest) -> Result<CompactionResult, CompactionError> {
        // 1. 构造 ProviderRequest：
        //    - context.system_prompt = self.summary_prompt
        //    - context.messages = [单条 User 消息，文本 = format_messages_for_summary(&req.messages)]（序列化，非原样透传）
        //    - context.tools = 空（摘要无需工具）
        //    - signal = req.signal
        // 2. 调用 provider.stream(request)：
        //    - 外层 Err → CompactionError::Provider
        //    - 流内累积 TextDelta → summary（忽略 ThinkingDelta / ToolCall*，摘要场景不应出现）
        //    - 流内 AssistantEvent::Error → CompactionError::Provider（映射为 ProviderError 语义，或新增包装；aborted=true 时仍走 Provider 错误）
        //    - 累积完毕（Done / 流自然结束）→ 若 summary 为空 → CompactionError::EmptySummary；否则 CompactionResult { summary }
        // 3. req.messages 为空 → CompactionError::EmptyInput
        // 4. 累积期间 select! 监听 req.signal.cancelled() → CompactionError::Cancelled
    }
}
```

- 摘要消息拼接规则：`req.messages` 按顺序拼接为可读文本（如 `role: content` 逐条），作为 provider 的 user 输入。具体拼接格式由实现自定，但必须**稳定、可单测断言**（在规格中给出默认格式，见下）。

### 编排逻辑（接入 context.rs / runtime.rs）

每轮 provider 请求前执行「上下文准备」：预算检查 → 必要时压缩 → 产出最终消息列表。

```rust
pub struct CompactionPolicy {
    pub budget_tokens: usize,   // 触发压缩的 token 阈值（估算）
    pub keep_recent: usize,     // 保留最近消息条数（不压缩）
}
```

编排流程（伪代码，实现落地于 context.rs，由 runtime 每轮调用）：

```
fn prepare_context(messages: Vec<Arc<Message>>, policy, compactor, signal) -> Vec<Arc<Message>> {
    if estimate_tokens(&messages) <= policy.budget_tokens {
        return messages;                              // 未超预算，不压缩
    }
    // 分界：前 (len - keep_recent) 条为待压缩，后 keep_recent 条保留
    let split = messages.len().saturating_sub(policy.keep_recent);
    if split == 0 { return messages; }                // 消息本就很少，不压缩
    let (to_compact, keep) = messages.split_at(split);

    match compactor.compact(CompactionRequest { messages: to_compact.to_vec(), signal }) {
        Ok(r) => {
            // 摘要注入为一条 User 消息，置于保留消息之前
            let summary_msg = Message::User(UserMessage { content: vec![Text(r.summary)], .. });
            [summary_msg] + keep
        }
        Err(_) => {
            // 降级：保守截断（丢弃最旧，等价于一期「超限截断」）
            keep.to_vec()
        }
    }
}
```

关键决策：

1. **摘要注入形式**：摘要作为一条普通 `User` 消息（`UserContent::Text(summary)`）置于保留消息之前，LLM 视作「历史摘要」。不新增 Message 变体，不破坏 002 定稿消息拓扑。
2. **降级语义**：压缩失败（Provider 错误 / 取消 / 空输入）**不阻断运行**，降级为保守截断（丢弃待压缩的旧消息，仅保留最近 `keep_recent` 条）。这与 003 一期「超限拒绝或保守截断」契约兼容。
3. **token 估算**：复用 context.rs 一期既有估算逻辑；若不存在，新增简单启发式（`chars/4` 兜底）并注明为粗估（与 003「粗估 token」一致）。
4. **接入点**：runtime 主循环在每轮 `stream_assistant_response` 构造 `ProviderRequest` **之前**调用 `prepare_context`。若 003 一期已在构造前有 `transform_context` 钩子调用点，`prepare_context` 作为其内部增强（先预算检查/压缩，再交给 transform_context 做通用投影）；语义与一期「超限截断」对齐。

### LoopConfig 变更（core/runtime.rs，二期扩展）

```rust
pub struct LoopConfig {
    // ...既有字段（model / convert_to_llm / transform_context / before_tool_call / ... 保持不变）
    pub compactor: Option<Arc<dyn Compactor>>,   // 二期新增：None = 关闭压缩（回退一期截断）
    pub compaction: CompactionPolicy,            // 二期新增：预算阈值 + 保留策略
}
```

- 新增字段会**破坏既有 `LoopConfig` 结构体字面量构造点**。Developer 需同步更新所有构造点（含 `tests/` 中的 FakeProvider 测试构造、`src/bin/` CLI），并跑通全量测试。若 `LoopConfig` 已有 `Default` impl，可用 `..Default::default()` 最小化改动；`CompactionPolicy` 提供 `Default`（保守默认：`budget_tokens` 大、`keep_recent` 小，等价于「几乎不压缩」）。

### 默认拼接格式（供单测断言）

LlmCompactor 把 `req.messages` 序列化为 provider user 输入，默认格式（稳定契约）：

```
[user] <content 拼接>
[assistant] <content 拼接>
[tool_result:<tool_name>] <content 拼接>
```

- 每条消息一行，`content` 内多段用 `\n` 拼接；`ToolCall` 段省略参数（摘要只关注语义）；`Thinking` 段忽略。实现可提供自定义 formatter，但默认格式必须如上，便于单测断言。

## Files

- src/core/compactor.rs（Compactor trait + LlmCompactor + CompactionError/CompactionRequest/CompactionResult + 单元测试）
- src/core/context.rs（修改：token 估算 + `prepare_context` 编排 + 单测）
- src/core/runtime.rs（修改：LoopConfig 增 `compactor`/`compaction` 字段，主循环每轮请求前接入 `prepare_context`）
- src/core/mod.rs（导出 Compactor 相关类型）
- src/lib.rs（re-export Compactor / LlmCompactor，若有 facade 导出惯例则同步）
- tests/compactor.rs（集成测试，fake provider 驱动）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets -D warnings passes
- [ ] cargo test --all-targets passes
- [ ] cargo fmt --check passes
- [ ] `LlmCompactor` 单测（fake provider 回放 `TextDelta* → Done`）：摘要文本正确累积；构造的 `ProviderRequest` 中 `system_prompt == summary_prompt`、`messages == [单条 User 输入，文本 = format_messages_for_summary(待压缩消息)]`、`tools` 为空
- [ ] `LlmCompactor` 错误路径：provider 外层 `Err` → `CompactionError::Provider`；流内 `Error` → `CompactionError::Provider`；空输入 → `EmptyInput`；空摘要（无 `TextDelta`，仅 `Done`/流自然结束）→ `EmptySummary`；`signal.cancel()` → `Cancelled`
- [ ] 编排 `prepare_context` 单测：未超预算不压缩（原样返回）；超预算压缩（旧消息被 `[user]` 摘要替换、最近 `keep_recent` 条保留）；压缩失败降级截断（仅保留最近 `keep_recent` 条）
- [ ] token 估算：复用或新增粗估（`chars/4` 兜底），有单测断言边界（空、短、长文本）
- [ ] 默认拼接格式：逐条 `[role] content` 输出稳定，有单测断言（含 User/Assistant/ToolResult 混合序列）
- [ ] 集成测试 `tests/compactor.rs`：构造超预算 transcript + fake provider → 压缩触发、摘要注入、后续请求携带压缩后 transcript；压缩失败 → agent 仍正常运行（降级截断）
- [ ] 产品代码无 `unwrap()`；异步测试用 `tokio::test`；不依赖外部服务（fake provider 全模拟）
- [ ] 单文件 ≤ 400 行，超则拆子模块并记录

## 修订记录

- v1.0（2026-08-27，Architect）：初稿。复用 003 定稿 ModelProvider/AssistantStream/AssistantEvent/ProviderRequest（不改签名）；Compactor 职责单一（生成摘要），编排（预算检查 + 保留策略 + 降级）落 context/runtime；LlmCompactor 持有 ModelProvider；压缩失败降级为保守截断（兼容一期契约）；摘要以普通 User 消息注入不破坏 002 消息拓扑；LoopConfig 加字段需同步更新既有构造点。
- v1.1（2026-08-28，Architect，依据 Developer 实现反馈修订）：消除正文与伪代码的自相矛盾——LlmCompactor 将待压缩消息**序列化为单条 User 输入**（默认 `format_messages_for_summary` 稳定格式，非原样透传）；① 伪代码注释「`context.messages = req.messages（原样）`」→「单条 User，文本 = format_messages_for_summary」；② 验收标准「`messages == 待压缩消息`」→「`messages == [单条 User 输入，文本 = format_messages_for_summary(待压缩消息)]`」；③ 新增 `CompactionError::EmptySummary` 变体——流结束未累积任何摘要文本视为错误并触发降级（规格原未定义此场景，防空摘要静默吞掉历史上下文）。
