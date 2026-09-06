# guigu 用户指南（中文）

> 状态：v0.1.0
> 本文档是 guigu 的中文用户文档，覆盖安装、快速开始、核心抽象与 **Provider 配置**。
> 架构细节见 [docs/architecture.md](architecture.md)，任务历史见 [docs/TASK_BOARD.md](TASK_BOARD.md)。

## 1. 简介

guigu 是一个轻量级、Rust 原生的 AI Agent 运行时。借鉴 [pi](https://github.com/earendil-works/pi) 的架构思想（非 1:1 移植），用 Rust 重写，追求性能、安全与极简依赖。它既能**作为库嵌入**你的应用，也能**作为独立 CLI** 运行。

核心能力：

- **Trait 抽象**：`Agent`、`Tool`、`ModelProvider`、`Plugin` 都是 trait，可组合内置实现或自行扩展。
- **Async-first**：基于 `tokio`，端到端非阻塞、可取消。
- **单写者运行时**：一个 runtime task 持有状态；`AgentHandle` 对外暴露命令、权威快照（`watch`）与增量事件（`broadcast`）。
- **内置工具**：文件 `read`/`write`/`edit` 与 `bash`，附带按路径的写串行化队列。
- **真实 LLM 适配器**：OpenAI 与 Anthropic（`reqwest`，feature-gated，rustls TLS）。
- **上下文管理**：token 预算、截断与 `Compactor` 摘要压缩。
- **会话树 + 崩溃恢复**：append-only JSONL 存储，支持 fork/reduce 与重放恢复。
- **多 lane 会话**：进程内多 lane 并发写同一棵会话树。
- **远程与协议**：NDJSON 远程协议、多 session `AgentServer`、ACP（Agent Client Protocol v1，stdio）。
- **CLI**：交互式 REPL 与 `--acp` 编辑器集成模式。
- **插件与惰性工具**：惰性实例化工具、异步初始化的插件注册。

## 2. 安装与 feature flags

在 `Cargo.toml` 中引入：

```toml
[dependencies]
guigu = "0.1.0"
```

### feature flags

| Feature | 默认 | 说明 |
|---------|------|------|
| `providers-http` | ✅ | OpenAI/Anthropic 适配器（`reqwest`，rustls TLS） |
| `acp-sse` | — | ACP 的 SSE/HTTP 传输（预留存根，见 roadmap） |

关闭默认 feature 可得到**不含 `reqwest` 的纯核心库**：

```toml
[dependencies]
guigu = { version = "0.1.0", default-features = false }
```

> 注意：`ModelProvider` trait 定义在核心（`core/provider.rs`），**始终可用、不受 `providers-http` 门控**。关闭默认 feature 后，你依然可以自行实现 `ModelProvider` 接入任意后端（见 §5.2）。

## 3. 快速开始

### 3.1 作为库嵌入

运行时遵循「单写者」模型：通过可 `Clone` 的 `AgentHandle` 驱动 agent，订阅增量事件流，从快照读取权威状态。

```rust
use guigu::{Agent, AgentHandle};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 启动唯一的 runtime task；`handle` 是可 Clone 的对外门面。
    let handle = AgentHandle::spawn(/* AgentConfig + tools */);

    // 2. 订阅增量事件流（broadcast）。
    let mut events = handle.subscribe();

    // 3. 通过命令队列发用户消息，驱动一轮。
    handle.prompt(vec![/* Message::User(...) */]).await?;

    // 4. 等待运行结束（AgentEnd 之后），读快照。
    handle.wait_for_idle().await?;
    let snapshot = handle.snapshot();

    // 5. 优雅关闭，等待 runtime task 退出。
    handle.shutdown().await?;
    Ok(())
}
```

> 以上为「生命周期示意」；可运行示例与权威 API 契约见 `tests/` 与 [docs/architecture.md](architecture.md) §3。

### 3.2 CLI 独立运行

构建并运行独立二进制：

```text
cargo run -- guigu run
```

```
guigu [OPTIONS] [COMMAND]

Commands:
  run      Interactive REPL (默认，可省略)
  acp      通过 stdio 提供 ACP 服务（编辑器子进程集成）
  help

Options:
  -m, --model <MODEL>      模型 id（如 gpt-4o-mini / claude-3-5-...）
  -p, --provider <PROVIDER>  openai | anthropic（默认 openai）
  -s, --session <ID>       加载/续接会话（不存在则新建）
  -c, --cwd <DIR>          工作目录（默认当前目录）
  -l, --log <DIR>          会话 JSONL 存储目录
  -k, --api-key <KEY>      Provider API key（缺省读 OPENAI_API_KEY / ANTHROPIC_API_KEY）
```

- `guigu run` 启动交互式 REPL，输入提示词后实时流式输出文本/工具进度；`/quit` 或 Ctrl-D 退出。
- `guigu acp` 通过 stdio 提供 Agent Client Protocol（JSON-RPC 2.0），供编辑器以子进程方式接入。

## 4. 核心抽象

### 4.1 Agent

`Agent` trait（`AgentHandle` 也实现了它）是驱动 agent 的入口：

```rust
#[async_trait]
pub trait Agent: Send + Sync {
    fn snapshot(&self) -> AgentSnapshot;
    fn subscribe(&self) -> broadcast::Receiver<AgentEvent>;
    async fn prompt(&self, messages: Vec<Message>) -> Result<(), AgentError>;
    async fn continue_(&self) -> Result<(), AgentError>;
    async fn steer(&self, msg: Message) -> Result<(), AgentError>;
    async fn follow_up(&self, msg: Message) -> Result<(), AgentError>;
    fn abort(&self);
    async fn wait_for_idle(&self) -> Result<(), AgentError>;
}
```

### 4.2 Tool

`Tool` trait 定义工具抽象，实现方式同样简单：

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Option<serde_json::Value>;
    fn resource_scope(&self) -> ResourceScope; // ReadOnly | FileWriter | Exclusive
    async fn execute(
        &self,
        tool_call_id: &str,
        args: serde_json::Value,
        signal: CancellationToken,
        on_update: Option<&dyn Fn(ToolResult)>,
    ) -> Result<ToolResult, ToolError>;
}
```

### 4.3 ModelProvider

`ModelProvider` 是**接入 LLM 后端的唯一抽象**，也是本文档 §5 的核心主题。

### 4.4 Plugin

`Plugin` trait + `PluginRegistry` 支持注册自定义工具与异步惰性实例化（详见 [docs/tasks/016-plugin-registry.md](tasks/016-plugin-registry.md)）。

## 5. Provider 配置（重点）

guigu 对 LLM 后端的接入分为**两层**，边界清晰：

- **内置 adapter 层**：开箱即用，仅 OpenAI / Anthropic 两个实现，`base_url` 可配置。
- **trait 层（嵌入库开放能力）**：`ModelProvider` 是公开扩展点，嵌入方自行实现即可接入任意后端。

### 5.1 内置 adapter 层：OpenAI / Anthropic（base_url 可配置）

库内置两个 adapter，位于 `src/adapters/`，由 `providers-http` feature 门控（默认开启）：

| Adapter | 协议 | 默认端点 |
|---------|------|----------|
| `OpenAiProvider` | OpenAI Chat Completions | `https://api.openai.com/v1` |
| `AnthropicProvider` | Anthropic Messages | `https://api.anthropic.com/v1` |

**两者的 `base_url` 均可覆盖默认端点**。这意味着任何提供「OpenAI 兼容」或「Anthropic 兼容」HTTP 接口的服务都能直接接入，**无需修改库代码**。

**关键理解**：「内置只有 OpenAI / Anthropic」指的是**协议适配器只有两种**，不是只能连这两家官方服务。`base_url` 可配置后，以下服务都能开箱即用：

- OpenAI 兼容（`/chat/completions`）：Ollama、vLLM、LM Studio、LocalAI、DeepSeek、Moonshot、通义千问、OpenRouter 等。
- Anthropic 兼容（`/messages`）：自建网关、代理。

**配置示例（OpenAI）**：

```rust
use guigu::adapters::{OpenAiConfig, OpenAiProvider};

// 方式一：官方端点（base_url 缺省）
let provider = OpenAiProvider::new(OpenAiConfig::new("sk-..."))?;

// 方式二：base_url 指向 OpenAI 兼容端点（如本地 Ollama）
let config = OpenAiConfig {
    api_key: "ollama".into(),
    base_url: Some("http://localhost:11434/v1".into()),
};
let provider = OpenAiProvider::new(config)?;
```

**配置示例（Anthropic）**：

```rust
use guigu::adapters::{AnthropicConfig, AnthropicProvider};

let config = AnthropicConfig {
    api_key: "sk-ant-...".into(),
    base_url: Some("https://gateway.example.com/v1".into()), // 自建网关 / 代理
    max_tokens: 4096,                                        // 默认 4096
    anthropic_version: "2023-06-01".into(),                  // 默认 2023-06-01
};
let provider = AnthropicProvider::new(config)?;
```

> 以上为「契约示意」，`OpenAiConfig` / `AnthropicConfig` 字段形状以 [docs/tasks/007-adapters.md](tasks/007-adapters.md) 规格与 `tests/adapters.rs` 为权威。

### 5.2 trait 层：自定义 provider（开放能力）

`ModelProvider` trait 定义于核心 `core/provider.rs`，是**公开扩展点**。嵌入方自行实现它，即可接入任意 LLM 后端——协议不兼容 OpenAI/Anthropic 的服务、私有模型服务、需要自定义鉴权/重试/缓存的场景——**完全无需修改库代码**。

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

**自定义示例（契约示意）**：

```rust
use guigu::{AssistantEvent, AssistantStream, ModelProvider, ProviderError, ProviderRequest};

struct MyProvider { /* 你的后端客户端 / 配置 */ }

#[async_trait::async_trait]
impl ModelProvider for MyProvider {
    async fn stream(
        &self,
        request: ProviderRequest,
    ) -> Result<AssistantStream, ProviderError> {
        // 1. 把 ProviderRequest（model / context / tools / signal）转成你后端的请求。
        // 2. 发送请求、解析响应。
        // 3. 返回标准化的 AssistantEvent 流：
        //    TextDelta / ThinkingDelta / ToolCallStart / ToolCallDelta / ToolCallEnd / Done / Error
        todo!()
    }
}
```

**错误两段式**（003 已定，自定义实现也必须遵守）：

- **请求建立失败**（网络不通、认证失败、参数非法）→ 外层返回 `Err(ProviderError)`。
- **流建立后的失败**（网络断、协议错、模型错）→ 流内产出 `AssistantEvent::Error { message, aborted }`，主循环据此生成终态 `stop_reason: Error`。

> `ModelProvider` trait 位于核心，**不受 `providers-http` feature 门控**。即使用 `default-features = false` 剥离了 `reqwest`，你依然可以实现自己的 provider。

### 5.3 如何选择

| 场景 | 选择 |
|------|------|
| OpenAI 官方 | 内置 `OpenAiProvider`（默认 `base_url`） |
| Anthropic 官方 | 内置 `AnthropicProvider`（默认 `base_url`） |
| OpenAI 兼容第三方（Ollama / vLLM / DeepSeek / Moonshot / OpenRouter…） | 内置 `OpenAiProvider` + 自定义 `base_url` |
| Anthropic 兼容网关 / 代理 | 内置 `AnthropicProvider` + 自定义 `base_url` |
| 协议不兼容（Gemini 原生等）/ 私有后端 / 特殊鉴权重试 | 自定义 `impl ModelProvider` |

## 6. 相关文档

- 架构设计（并发模型、消息/事件模型、工具编排、模块布局）：[docs/architecture.md](architecture.md)
- 任务历史与状态：[docs/TASK_BOARD.md](TASK_BOARD.md)
- 下一阶段路线图：[docs/roadmap.md](roadmap.md)
- 英文简介 / crates.io 首页：[../README.md](../README.md)

## License

MIT，见 [../LICENSE](../LICENSE)。
