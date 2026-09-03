# Task 014: ACP 适配（Agent Client Protocol v1）

## Background

010/013 已提供 guigu 自定义的远程协议与多 session server。但要让 guigu 被**标准编辑器/客户端**（Zed 等）直接使用，需要实现 **ACP（Agent Client Protocol v1）**——一套开放的 JSON-RPC 2.0 契约，把「编辑器（Client）」与「编码 Agent（Agent）」解耦。

ACP 是双工协议：guigu 作为 **agent 侧**，接收 client→agent 方法（initialize / session/*），并主动向 client 发起 agent→client 方法（`session/update`、`fs/read_text_file`、`fs/write_text_file`、`session/request_permission`）。传输层分两类：**stdio**（本地，1 进程 = 1 client）与 **SSE+HTTP**（远程，多 client）。

> 本规格以 ACP v1 stable 官方规范为 wire 权威（字段名/版本/枚举以官方 spec 为准），guigu 只实现其**最小可用面**，并把内部类型映射到 ACP。

## Goal

- 定义 `AcpAgent` 适配器：实现 client→agent 方法 + 通过 client 传输句柄发起 agent→client 调用
- 实现 ACP 方法映射：initialize / authenticate(可选) / session/new / session/load / session/prompt / session/cancel / session/set_mode / session/update / fs/read_text_file / fs/write_text_file / session/request_permission
- 传输层：stdio（必做）+ SSE+HTTP（多 client，feature-gated）
- 复用 013 `AgentServer` 做 session/lane 后端，007 adapters 做模型，005/006 做工具（工具具体装配由 015 CLI / 嵌入方传入）

## Design Notes

### 方法映射（agent 侧实现）

| ACP 方法（方向） | guigu 映射 |
|---|---|
| `initialize`（c→a） | 返回 `protocolVersion` + `AgentCapabilities`（`loadSession: true`、`promptCapabilities.text`、`fs: true`、`mcpCapabilities: 无`、`background: false`） |
| `authenticate`（c→a） | 一期不支持 auth → 返回 `authMethods: []`（initialize 已声明无 auth） |
| `session/new`（c→a） | `server.create_session` + 分配 sessionId → 返回 `{ sessionId }` |
| `session/load`（c→a） | `server.load_session`（从持久化恢复）→ 返回 `{ sessionId, ... }` |
| `session/prompt`（c→a） | `server.prompt(session, lane=default)`；把 001 `AgentEvent` 流映射为 `session/update` 推送；结束返回 `PromptResponse { stopReason }` |
| `session/cancel`（c→a，notification） | `server.abort(session, lane)` |
| `session/set_mode`（c→a，notification） | 记录 permission mode（`default/acceptEdits/plan/bypassPermissions/dontAsk/auto`），影响后续 fs/权限判定 |
| `session/update`（a→c，notification） | 把 `AgentEvent` 映射为 `SessionUpdate` 变体（text→`agent_message_chunk`、thinking→`agent_thought_chunk`、ToolCall→`tool_call`、ToolResult→`tool_call_update`） |
| `fs/read_text_file`（a→c） | 工具读文件时经 client 代理（权限隔离）；`AgentCapabilities.fs` 声明后可用 |
| `fs/write_text_file`（a→c） | 工具写文件时经 client 代理 |
| `session/request_permission`（a→c） | 工具执行前，mode=default/plan 时向 client 请求权限；`bypassPermissions/acceptEdits` 时直接放行 |

### 核心类型（src/acp/mod.rs）

```rust
/// ACP agent 侧适配器：实现 client→agent 方法，并经 client_handle 发起 agent→client 调用。
pub struct AcpAgent {
    server: AgentServer,                       // 013 多 session 后端
    mode: Arc<tokio::sync::RwLock<PermissionMode>>, // 当前权限模式（session/set_mode 更新）
}

/// 由传输层注入的「调用 client 方法」句柄（stdio/SSE 各自实现）。
pub trait AcpClient {
    /// 向 client 发请求（如 fs/read_text_file、session/request_permission），返回 JSON-RPC 结果。
    async fn request(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, AcpError>;
    /// 向 client 发 notification（如 session/update）。
    async fn notify(&self, method: &str, params: serde_json::Value) -> Result<(), AcpError>;
}

impl AcpAgent {
    pub fn new(server: AgentServer) -> Self;
    /// 处理一条 client→agent JSON-RPC 请求（dispatch 到上表方法）。
    pub async fn handle(&self, client: &dyn AcpClient, method: &str, params: serde_json::Value)
        -> Result<serde_json::Value, AcpError>;
}
```

- **双工要点**：`handle` 在收到 `session/prompt` 时，需「边跑 runtime 边向 client 推 `session/update`」——实现为：spawn 一个 task 消费 `server.subscribe(session, lane)` 事件流，逐条 `client.notify("session/update", ...)`，`session/prompt` 主请求在 lane run 结束后返回 `PromptResponse`。
- `PermissionMode` 用 `serde_repr` 或 `#[serde(rename_all)]` 对齐 ACP 枚举；`stopReason` 映射：001 的 `stop_reason`（completed/length/error/aborted）→ ACP `end_turn/max_tokens/refusal/cancelled`（无直接对应的按最相近映射，映射表写入 doc 注释）。

### 传输层（src/acp/transport.rs）

```rust
impl AcpAgent {
    /// stdio：对 stdin/stdout 跑 JSON-RPC 2.0（1 进程 = 1 client）。
    pub async fn serve_stdio(self) -> Result<(), AcpError>;
}

#[cfg(feature = "acp-sse")]
impl AcpAgent {
    /// SSE+HTTP：远程多 client；`AgentServer` 多连接由 HTTP 层复用（每个 SSE 连接 = 一个 client）。
    pub async fn serve_sse(self, addr: std::net::SocketAddr) -> Result<(), AcpError>;
}
```

- **stdio**：JSON-RPC 2.0 消息分帧（`Content-Length` 头或 newline-delimited，以 ACP 官方 stdio 约定为准）；读循环 dispatch `handle`，写半单 writer task。
- **SSE+HTTP**：feature-gated（`acp-sse`），依赖 `axum`/`reqwest`（或最小 HTTP 栈，Developer 评估，新增依赖需在 Cargo.toml 声明并在修订记录说明）。本任务 stdio 为必做验收项，SSE 为加分项（可降级为后续）。

### 错误语义（src/acp/mod.rs）

```rust
#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    #[error("json-rpc error: {0}")]        JsonRpc(String),
    #[error("unsupported method: {0}")]    UnsupportedMethod(String),
    #[error("server error: {0}")]          Server(#[from] ServerError),
    #[error("io error: {0}")]              Io(#[from] std::io::Error),
    #[error("serialize error: {0}")]       Serde(#[from] serde_json::Error),
}
```

### 边界声明（明确不做）

- **authenticate / 多认证方法**：一期不支持，`authMethods: []`。
- **MCP 集成**（initialize 携带 MCP server）：不做，`mcpCapabilities` 缺省。
- **terminal 方法**（ACP terminal 扩展）：不在本任务（属后续/插件）。
- **SSE+HTTP 为可选加分项**：默认 feature 不含 `acp-sse`；stdio 为必做 DoD。
- **工具装配**：AcpAgent 不内置模型/工具；由 015 CLI 或嵌入方经 `AgentServer.spawn_lane(runtime)` 注入（fs 工具改为经 `AcpClient` 代理读写的 `AcpFsTool`，本任务提供其骨架）。

## Files

- src/acp/mod.rs（`AcpAgent`/`AcpClient`/`PermissionMode`/`AcpError` + 方法映射 + 单测）
- src/acp/transport.rs（`serve_stdio` + JSON-RPC 分帧 + 单测；`serve_sse` 在 `acp-sse` feature 下）
- src/acp/fs_tool.rs（`AcpFsTool`：fs/read_text_file、fs/write_text_file 经 `AcpClient` 代理，实现 `Tool` trait）
- src/lib.rs（登记 `pub mod acp` + re-export `AcpAgent`/`AcpError`；feature `acp-sse` 声明）
- tests/acp.rs（集成测试：stdio loopback + fake client 驱动完整 initialize→session/new→session/prompt→session/update→stopReason 链路）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets -D warnings passes
- [ ] cargo test --all-targets passes
- [ ] cargo fmt --check passes
- [ ] 方法映射单测：initialize 返回合法 `AgentCapabilities`；session/new 返回 sessionId；session/prompt 收到 `session/update` 序列并返回 `PromptResponse.stopReason`；session/cancel 触发 lane abort；set_mode 更新权限模式
- [ ] 事件映射单测：`AgentEvent`（text/thinking/tool_call/tool_result）→ 对应 `SessionUpdate` 变体；stop_reason → ACP 映射正确
- [ ] `AcpFsTool`：读/写经 `AcpClient` 代理（fake client 断言收到 `fs/read_text_file`/`fs/write_text_file`），权限 mode=plan 时先发 `session/request_permission`
- [ ] stdio loopback 集成测试（duplex + fake client）：完整 ACP 会话往返，事件逐条推、最终 stopReason 正确
- [ ] 产品代码无 `unwrap()`；异步测试用 `tokio::test`；JSON-RPC 分帧 roundtrip 测试
- [ ] 单文件 ≤ 400 行，超则拆子模块并记录

## 修订记录

- v1.0（2026-09-01，Architect）：初稿。ACP v1 stable 为 wire 权威（官方 spec 为准）；guigu 实现最小可用面（initialize/session/new/load/prompt/cancel/set_mode + session/update + fs/read+write + request_permission）；双工经 `AcpClient` 句柄注入；stdio 必做、SSE 可选加分项；工具装配下沉到 015 CLI/嵌入方，fs 工具经 client 代理（权限隔离）。
- v1.1（2026-09-03，Architect，依据 r4 审查建议）：统一 stopReason 措辞——正文将 `cancellation` 更正为官方 wire 值 `cancelled`（`end_turn/max_tokens/refusal/cancelled`），消除规格与实现/官方 spec 的措辞歧义。
