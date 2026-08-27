# Task 007: adapters（OpenAI / Anthropic，reqwest feature-gated）

## Background

001–006 已交付消息/事件、AgentHandle、Runtime 主循环（fake provider 驱动）与内置工具。当前 runtime 只能被 `FakeProvider` 驱动，无法接真实 LLM。本任务落地 `ModelProvider` trait 的两个**真实 HTTP 实现**：OpenAI 兼容 Chat Completions API 与 Anthropic Messages API。008（上下文压缩 Compactor）依赖本任务提供的真实 LLM 摘要能力，故 007 必须在 008 之前完成。

核心价值：把内部规范化的 `ProviderRequest` 转换为各 provider 的 HTTP 请求，把 HTTP SSE 响应流解析并映射为 `AssistantEvent` 流。所有格式转换与流解析逻辑必须**可纯单元测试**（不依赖网络）。

## Goal

- 新增 `src/adapters/`：公共 SSE 解析器 + 两个 `ModelProvider` 实现（OpenAI / Anthropic）
- `reqwest` 作为 optional dependency，经 `providers-http` feature 门控；`adapters` 模块整体 `#[cfg(feature = "providers-http")]`
- 请求构造、SSE 解析、事件映射三层均为纯逻辑，可脱离网络单测；端到端用 wiremock（dev-dependency）起本地 mock server 验证真实 HTTP 格式

## Design Notes

### 契约复用（以 003 定稿为准，勿改签名）

`ModelProvider` / `AssistantStream` / `AssistantEvent` / `ProviderRequest` 均已在 `core/provider.rs`（003 定稿）定义，007 **不修改其签名**，只提供实现：

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

- **Model / Context / ProviderError 的具体字段形状以 `core/provider.rs` 003 实际实现为准**。Developer 在实现前先核对这三个类型的真实结构，按语义提取所需数据（下表），不得新增/修改其既有公开字段（ProviderError 缺变体除外，见下）。
- 从 `ProviderRequest` 提取的语义（字段名与实现对齐，语义固定）：

| 需求 | 来源 | 用途 |
|------|------|------|
| model 标识字符串 | `request.model` 的 id 字符串 | 透传为 provider 请求体 `model` 字段 |
| system prompt | `request.context` 的 system prompt | OpenAI `messages[0]`(role=system) / Anthropic 顶层 `system` |
| 消息序列 | `request.context` 的 messages | 逐条映射为 provider messages（见映射表） |
| 工具定义 | `request.context` 的 tools（每项有 name/description/parameters） | OpenAI `tools` 数组 / Anthropic `tools` 数组 |
| 取消 | `request.signal` | 传输/解析期间 `select!` 打断，中止流 |

### ProviderError 语义（若 003 未覆盖则补齐）

adapter 需表达四类错误；若 003 已定义 `ProviderError` 且变体不同，复用既有变体并保证能表达以下语义；若缺变体，在 `core/provider.rs` 按 thiserror 补齐（**不破坏既有变体与构造器**）：

| 分类 | 触发 | 建议变体 |
|------|------|---------|
| 网络 | 连接失败 / 超时 | `Network(String)` |
| HTTP 非 2xx | 401 认证 / 400 参数 / 429 限流 / 5xx 服务端 | `HttpStatus { status: u16, body: String }` |
| 解析 | SSE / JSON 结构非法 | `Parse(String)` |
| 构造 | 请求体构造失败（防御性，正常不应发生） | `Build(String)` |

错误两段式（003 已定）：请求建立失败 → 外层 `Err(ProviderError)`；流建立后的失败 → 流内 `AssistantEvent::Error { message, aborted }`。

### 模块结构 + feature 门控

```
src/adapters/
├── mod.rs          # pub mod sse/openai/anthropic + re-export；整个模块 #[cfg(feature = "providers-http")]
├── sse.rs          # 通用 SSE 解析器（纯逻辑，字节行流 → 事件枚举）
├── openai.rs       # OpenAiProvider：build_request + map_event + stream
└── anthropic.rs    # AnthropicProvider：同上
```

- `src/adapters/mod.rs` 顶部 `#![cfg(feature = "providers-http")]`（或对每个子模块 `#[cfg]`）。
- `Cargo.toml` 变更：
  ```toml
  [features]
  default = ["providers-http"]
  providers-http = ["dep:reqwest"]

  [dependencies]
  reqwest = { version = "0.12", features = ["json", "stream"], optional = true }
  futures = { version = "0.3", default-features = false, features = ["std"] }  # 若已存在则复用，保证 StreamExt 可用

  [dev-dependencies]
  wiremock = "0.6"
  ```
- `default = ["providers-http"]` 的理由：DoD 门禁 `cargo test`（无 `--features`）需覆盖 adapter 测试，否则 adapter 测试被跳过违背"测试执行真实逻辑"。嵌入方用 `default-features = false` 即可得到无 reqwest 的核心库（在 `lib.rs` 顶部 doc comment 注明）。

### 公共 SSE 解析器（sse.rs，纯逻辑）

```rust
pub enum SseEvent {
    Named { event: String, data: String },   // 有 event: 行
    Data { data: String },                   // 仅 data: 行
    Done,                                    // data: [DONE]
}
pub fn parse_sse_line<'a>(...) -> ...        // 状态机：按空行分隔事件，data 多行拼接，[DONE] 终止
```

解析规则：以空行分隔事件；`data:` 多行用 `\n` 拼接；`event:` 行指定事件名；`data: [DONE]` 产出 `Done`；忽略 `:` 开头的注释行与未知字段行；兼容 `\r\n`。

### 请求构造（build_request，纯函数，产出 URL + headers + JSON body）

**OpenAiProvider**：
- URL：`{base_url}/chat/completions`（base_url 默认 `https://api.openai.com/v1`）
- headers：`Authorization: Bearer {api_key}`、`Content-Type: application/json`
- body：
  ```json
  {
    "model": "<透传 model 字符串>",
    "messages": [ ... ],
    "tools": [ {"type":"function","function":{"name":"..","description":"..","parameters":{..}}} ],
    "stream": true,
    "stream_options": {"include_usage": true}
  }
  ```
  - `tools` 为空时省略该字段。
- 消息映射（内部 → OpenAI）：
  | 内部 | OpenAI |
  |------|--------|
  | User（纯文本） | `{"role":"user","content":"<拼接文本>"}` |
  | User（含 Image） | `{"role":"user","content":[{"type":"text",...},{"type":"image_url","image_url":{"url":"data:{mime};base64,{data}"}}]}`（基础映射） |
  | Assistant | `{"role":"assistant","content":"<拼接Text>或null","tool_calls":[{id,type:"function",function:{name,arguments}}]}`（Thinking 段忽略） |
  | ToolResult | `{"role":"tool","tool_call_id":"..","content":"<拼接Text>"}` |

**AnthropicProvider**：
- URL：`{base_url}/messages`（base_url 默认 `https://api.anthropic.com/v1`）
- headers：`x-api-key: {api_key}`、`anthropic-version: {version}`（默认 `2023-06-01`）、`Content-Type: application/json`
- body：
  ```json
  {
    "model": "<透传>",
    "max_tokens": 4096,
    "system": "<system prompt>",
    "messages": [ ... ],
    "tools": [ {"name":"..","description":"..","input_schema":{..}} ],
    "stream": true
  }
  ```
- 消息映射（内部 → Anthropic，content 为 content blocks 数组）：
  | 内部 | Anthropic |
  |------|-----------|
  | User（纯文本） | `{"role":"user","content":[{"type":"text","text":".."}]}` |
  | User（含 Image） | 追加 `{"type":"image","source":{"type":"base64","media_type":..,"data":..}}`（基础映射） |
  | Assistant | `{"role":"assistant","content":[{"type":"text","text":".."},{"type":"tool_use","id":"..","name":"..","input":<解析后的JSON对象>}]}`（Thinking 段忽略；`input` 由 arguments JSON 字符串反序列化为对象） |
  | ToolResult | `{"role":"user","content":[{"type":"tool_result","tool_use_id":"..","content":"<拼接Text>","is_error":bool}]}`（每个 ToolResult 独立一条 user 消息） |

### 事件映射（map_event，纯函数）

**OpenAI（SSE `data:` 为 `{choices:[{delta,finish_reason}],usage}` chunk）**：

| 输入 | 输出 AssistantEvent |
|------|---------------------|
| `delta.content` 非空 | `TextDelta { text }` |
| `delta.reasoning_content` 非空（若存在） | `ThinkingDelta { thinking }` |
| `delta.tool_calls[i].id` 非空 | `ToolCallStart { id, name: function.name, arguments: "" }` |
| `delta.tool_calls[i].function.arguments` 非空 | `ToolCallDelta { id, arguments_delta }` |
| `finish_reason == "tool_calls"` | 对每个未结束 tool_call 发 `ToolCallEnd { id }` |
| `usage` 非空（末 chunk） | 记录 Usage（见下），不单独发事件 |
| `finish_reason` 非 null | 记录 StopReason（见下） |
| `data: [DONE]` | 触发流结束 → 累积组装 `Done { message }` |

**Anthropic（SSE `event:` 区分类型，`data:` 为 JSON）**：

| 输入（data.type） | 输出 AssistantEvent |
|-------------------|---------------------|
| `content_block_start`(text) | （无，等 delta） |
| `content_block_start`(tool_use) | `ToolCallStart { id: content_block.id, name: content_block.name, arguments: "" }` |
| `content_block_start`(thinking) | （无，等 delta） |
| `content_block_delta`(text_delta) | `TextDelta { text }` |
| `content_block_delta`(thinking_delta) | `ThinkingDelta { thinking }` |
| `content_block_delta`(input_json_delta) | `ToolCallDelta { id, arguments_delta: partial_json }` |
| `content_block_stop`(tool_use) | `ToolCallEnd { id }` |
| `message_delta` | 记录 `stop_reason` + `usage` |
| `message_stop` | 触发流结束 → 累积组装 `Done { message }` |

### 累积规则（adapter 内部，产出 Done 的完整 AssistantMessage）

adapter 维护内部累积状态 `Acc { text, thinking, tool_calls: Vec<ToolCallAcc{id,name,arguments}>, usage, stop_reason, model }`：

1. `TextDelta`/`ThinkingDelta` 按到达顺序追加到 `text`/`thinking`（并记录其相对于 tool_calls 的顺序，用于最终 content 排序）。
2. `ToolCallStart` 追加新 `ToolCallAcc`（arguments 空）；`ToolCallDelta` 按 id 找到对应项 `arguments += delta`；`ToolCallEnd` 标记该项完成。
3. 流结束（`[DONE]` / `message_stop`）→ 构造 `AssistantMessage`：
   - `content`：按 provider 给出的顺序排列（Anthropic 按 `content_block` 的 `index` 升序；OpenAI 按 text/thinking/tool_call 首次出现顺序），ToolCall 段用累积完成的 `ToolCallAcc` 生成 `AssistantContent::ToolCall(ToolCall{id,name,arguments})`。
   - `usage`：映射后填入；`stop_reason`：映射后填入；`model`：`Some(ModelId)`（透传模型字符串）；`error_message`：None。
4. 若流在 `[DONE]`/`message_stop` 之前被取消或中断 → 发 `AssistantEvent::Error { message, aborted }`（取消为 `aborted: true`，网络/协议中断为 `false`），不发 `Done`。

**Usage 映射**：
| 内部字段 | OpenAI | Anthropic |
|----------|--------|-----------|
| input | `prompt_tokens` | `input_tokens` |
| output | `completion_tokens` | `output_tokens` |
| cache_read | `prompt_tokens_details.cached_tokens`（可空） | `cache_read_input_tokens`（可空） |
| cache_write | 无 → None/0 | `cache_creation_input_tokens`（可空） |
| total_tokens | `total_tokens` | `input_tokens + output_tokens` |
| cost | None（无定价表） | None |

**StopReason 映射**：
| 内部 StopReason | OpenAI finish_reason | Anthropic stop_reason |
|-----------------|----------------------|-----------------------|
| Completed | `stop` / `tool_calls` | `end_turn` / `stop_sequence` / `tool_use` |
| Length | `length` | `max_tokens` |
| Error | `content_filter` | `refusal` |
| Other(s) | 其它未知值 | 其它未知值 |
| （流中） | null 不设 | null 不设 |

### 取消

- `stream()` 建立请求阶段：`tokio::select!` 于 `request.signal.cancelled()`，取消 → 返回 `Err(ProviderError::Network("cancelled".into()))`（或专用取消语义）。
- 流建立后：将 `request.signal` 传入流，流的每一轮 `next()` 用 `select!` 打断，取消 → 发 `AssistantEvent::Error { aborted: true }` 后终止流。
- 实现方式：请求体构造时 `signal` 已传入 `ProviderRequest`；用 `futures::StreamExt` + `async_stream`/`select` 组合，不引入新依赖（可用 `tokio::select!` 在自定义 Stream 适配器内实现）。

### 构造与配置

```rust
pub struct OpenAiConfig { pub api_key: String, pub base_url: Option<String> }
impl OpenAiConfig { pub fn new(api_key: impl Into<String>) -> Self; }
pub struct OpenAiProvider { /* reqwest::Client + config */ }
impl OpenAiProvider {
    pub fn new(config: OpenAiConfig) -> Result<Self, ProviderError>;  // 构造 reqwest Client
}

pub struct AnthropicConfig {
    pub api_key: String,
    pub base_url: Option<String>,
    pub max_tokens: u32,              // 默认 4096
    pub anthropic_version: String,    // 默认 "2023-06-01"
}
impl AnthropicConfig { pub fn new(api_key: impl Into<String>) -> Self; }
pub struct AnthropicProvider { /* ... */ }
impl AnthropicProvider { pub fn new(config: AnthropicConfig) -> Result<Self, ProviderError>; }
```

- `reqwest::Client` 注入 provider 内部；base_url 默认为官方端点，测试时覆盖为 wiremock 地址。
- `lib.rs` 补 re-export：`pub use adapters::{openai::*, anthropic::*}`（在 `#[cfg(feature = "providers-http")]` 下）。

### 测试策略

1. **纯逻辑单测（`#[cfg(test)]`，无网络）**：build_request（断言 URL/headers/body JSON 形状）、SSE 解析（多行 data、event+data、`[DONE]`、空行分隔、CRLF）、事件映射（上述映射表逐条）、累积（文本拼接、多 tool_call 按 id 分组 arguments、content 顺序、stop_reason/usage 映射）。
2. **端到端（wiremock，`tests/adapters.rs` 加 `#![cfg(feature = "providers-http")]`）**：mock server 返回预置 SSE 流 → 断言 `AssistantStream` 产出完整事件序列 + `Done` 的 message（content/usage/stop_reason 正确）；HTTP 401 → `ProviderError::HttpStatus`；`signal.cancel()` 后流中止并产出 `Error{aborted:true}`。
3. 测试不依赖外部服务（wiremock 走 localhost）；API key 用假值。

## Files

- src/adapters/mod.rs（`#![cfg(feature = "providers-http")]` + 子模块声明 + re-export）
- src/adapters/sse.rs（SSE 解析器 + 单元测试）
- src/adapters/openai.rs（OpenAiProvider + OpenAiConfig + build_request/map_event + 单元测试）
- src/adapters/anthropic.rs（AnthropicProvider + AnthropicConfig + build_request/map_event + 单元测试）
- src/lib.rs（`#[cfg(feature = "providers-http")]` re-export adapters；顶部 doc comment 注明 `default-features = false` 剥离 HTTP）
- Cargo.toml（`[features]` + reqwest optional + wiremock dev-dependency）
- tests/adapters.rs（wiremock 端到端，`#![cfg(feature = "providers-http")]`）
- src/core/provider.rs（**仅当** ProviderError 缺四类变体时补齐；不改 ModelProvider 等既有签名）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets -D warnings passes
- [ ] cargo test --all-targets passes（default 含 providers-http，adapter 测试全量跑）
- [ ] cargo fmt --check passes
- [ ] `cargo test --no-default-features` 下核心库编译通过、adapter 测试被 feature 门控跳过（验证 default-features=false 可剥离 reqwest）
- [ ] build_request：OpenAI body 含 `model`/`messages`/`tools`/`stream:true`/`stream_options.include_usage`；Anthropic body 含 `model`/`max_tokens`/`system`/`messages`/`tools(input_schema)`/`stream:true`；消息映射表逐条正确（User/Assistant/ToolResult → 各 provider role）
- [ ] SSE 解析：多行 data 拼接、event+data、`[DONE]`、空行分隔、CRLF 兼容
- [ ] 事件映射：OpenAI text/tool_call start/delta/end/usage/finish_reason、Anthropic content_block_*/message_delta/message_stop 逐条映射为对应 AssistantEvent
- [ ] 累积：文本拼接、多 tool_call 按 id 分组 arguments 累积、content 顺序、stop_reason/usage 映射正确，Done 的 AssistantMessage 完整
- [ ] 端到端（wiremock）：完整 SSE 流 → 事件序列 + Done message 正确；HTTP 401 → `ProviderError::HttpStatus`；`signal.cancel()` → 流中止 + `Error{aborted:true}`
- [ ] 错误两段式：请求建立失败走外层 `Err`，流内失败走 `AssistantEvent::Error`
- [ ] 产品代码无 `unwrap()`；测试内用 `expect("前置条件")`；API key 用假值，不依赖外部服务
- [ ] 单文件 ≤ 400 行，超则拆子模块并记录

## 修订记录

- v1.0（2026-08-27，Architect）：初稿。复用 003 定稿 `ModelProvider`/`AssistantStream`/`AssistantEvent`/`ProviderRequest`；错误两段式 + ProviderError 四类语义；default feature 含 providers-http（保证 DoD `cargo test` 覆盖 adapter，嵌入方 `default-features=false` 剥离 HTTP）；SSE/请求构造/事件映射/累积四层纯逻辑 + wiremock 端到端。
