# Task 002: Message/Event 数据结构

## Background

guigu 是 Rust 原生的 AI Agent 运行时，消息与事件是全部上层逻辑的地基。参考 Pi 的判别联合消息模型，用 Rust enum 表达更精确、类型安全。本任务先行交付数据层，供 001/003/004 依赖。

## Goal

实现 `src/core/message.rs`（消息、内容段、usage、stop reason）与 `src/core/event.rs`（生命周期事件），并保证 serde 序列化 roundtrip。

## Design Notes

### 消息模型（定稿，废弃过宽 Content）

ToolResult 必须是**顶层消息**，不嵌入 Content。合法拓扑：

```
User -> Assistant(ToolCall) -> ToolResult -> Assistant
```

```rust
// src/core/message.rs
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

pub struct ImageContent {
    pub data: String,          // base64
    pub mime_type: String,
}

pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,     // JSON 字符串（流式累积期可为部分）
}

pub struct UserMessage {
    pub content: Vec<UserContent>,
    pub timestamp: u64,
}

pub struct AssistantMessage {
    pub content: Vec<AssistantContent>,
    pub model: Option<ModelId>,
    pub usage: Option<Usage>,
    pub stop_reason: Option<StopReason>,
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

辅助类型：
- `ModelId`（newtype String）
- `Usage { input, output, cache_read, cache_write, total_tokens, cost }`
- `StopReason`：`Completed | Length | Error | Aborted | Pending | Other(String)`（serde 用 tagged enum，兼容任意未知字符串走 `Other`）
- `ThinkingLevel`：`Off | Minimal | Low | Medium | High | Xhigh | Max`

### 事件模型

```rust
// src/core/event.rs
pub enum AgentEvent {
    AgentStart,
    AgentEnd { messages: Vec<Arc<Message>> },
    TurnStart,
    TurnEnd { message: Arc<AssistantMessage>, tool_results: Vec<ToolResultMessage> },
    MessageStart { message: Arc<Message> },
    MessageUpdate { message: Arc<Message>, assistant_event: AssistantEvent },
    MessageEnd { message: Arc<Message> },
    ToolExecutionStart { tool_call_id: String, tool_name: String, args: serde_json::Value },
    ToolExecutionUpdate { tool_call_id: String, tool_name: String, args: serde_json::Value, partial: ToolResult },
    ToolExecutionEnd { tool_call_id: String, tool_name: String, result: ToolResult, is_error: bool },
}
```

- `ToolResult`（工具产出，供事件与 Tool trait 复用，放 `event.rs` 或独立 `tool_result.rs`，由实现者定，避免循环依赖）
- `AssistantEvent` 属 provider 流事件，**本任务不实现**（003 的 provider.rs），event.rs 只在 `MessageUpdate` 中引用其类型——若产生循环依赖，则将 `AssistantEvent` 的占位类型放 event.rs 或 provider.rs 由实现者裁定并记录。

### 序列化约束

- 所有类型 `#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]`
- 测试必须覆盖每种消息/内容段/事件 roundtrip
- serde tag 策略：`Message`/内容段用 `#[serde(tag = "type", rename_all = "snake_case")]` 与 Pi JSON 形状保持一致

### 文件拆分

`message.rs` 若超 400 行，拆为 `core/message/mod.rs` + `content.rs` + `usage.rs`，由实现者按 responsibility 拆分并记录。

## Files

- src/core/message.rs
- src/core/event.rs
- src/core/mod.rs（登记模块）
- tests/message.rs

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy -D warnings passes
- [ ] cargo test passes
- [ ] cargo fmt --check passes
- [ ] tests/message.rs 覆盖：User/Assistant/ToolResult 序列化 roundtrip，内容段全枚举，StopReason 未知值兜底，AgentEvent 全变体 roundtrip
- [ ] 无 `unwrap()`（用 `thiserror` 错误或 `assert!`/`assert_eq!`）
