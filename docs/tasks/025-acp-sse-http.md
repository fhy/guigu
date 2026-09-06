# Task 025: ACP SSE/HTTP 远程多 client

## Background

014 已交付 ACP v1 的 stdio 传输（1 进程 = 1 client），但将 SSE+HTTP 降级为存根——`src/acp` 的 `serve_sse` 仅声明未实现（`#[cfg(feature = "acp-sse")]`）。这使 guigu 只能被本地单客户端使用，无法支撑编辑器远程多 client 并发接入（roadmap 候选 #2）。

本任务补齐 SSE+HTTP 传输层：复用 013 `AgentServer`（多 session 后端）+ 014 `AcpAgent`/`AcpClient`/JSON-RPC codec，起一个 HTTP server（SSE 事件流 + JSON-RPC 消息端点），支持多 client 并发接入、session 隔离、agent→client 双工请求（`fs/*`、`session/request_permission`）。

新依赖 `axum`（+ SSE 流 helper）feature-gated 在既有 `acp-sse` feature（非 default，**PM 已签核「批准 feature-gated」**）。

## Goal

- 实现 `AcpAgent::serve_sse(self, addr)`（014 存根签名）：axum HTTP server，暴露 `GET /sse`（agent→client 事件流）+ `POST /message`（client→agent JSON-RPC）。
- 多 client 模型：每个 SSE 连接 = 一个 client，注册进进程内 `ClientRegistry`（`client_id → mpsc::Sender<SseEvent>`）。
- SSE 版 `AcpClient`：`notify` 经 mpsc 推 SSE；`request` 经 pending map 关联 POST 响应（超时兜底）。
- 复用 014 `AcpAgent`（含 `handle` dispatch + 方法映射）+ 013 `AgentServer`；修正 permission mode 为 per-session（多 client 串扰修复）。
- feature：`acp-sse = ["dep:axum", "dep:tokio-stream"]`（非 default）。

## Design Notes

### 1. 传输模型（SSE + HTTP 双工）

wire 以 **ACP v1 官方 SSE transport 规范为权威**（端点/字段/错误码以官方 spec 为准）；若官方对该传输未定稿，则 guigu 采用以下最小约定并在 doc 注释说明：

- `GET /sse`：建立 agent→client 事件流。服务器生成 `client_id`（复用 013 既有 id 生成或 `AtomicU64` 单调计数，**不新增 `uuid` crate**），注册进 `ClientRegistry`，首个 SSE event 回 `{client_id}`（供后续 `POST /message` 定位 client）。此后 agent→client 的 request/notification 均从该流推出。
- `POST /message`：client→agent。body 为 JSON-RPC 2.0 消息（request/notification/response），携带 `client_id`（header 或 query，Developer 定）。dispatch 到 `AcpAgent::handle`；若 body 是对某 pending request 的响应（`id` 命中 pending map）则 resolve。

SSE event 格式：`event: <method>`、`data: <JSON-RPC payload>`。agent→client 的「request」（需响应，如 `fs/read_text_file`）与「notification」（如 `session/update`）都经 SSE 推出；request 的响应由客户端 `POST /message` 回传（标准双工）。

### 2. 多 client 注册表（src/acp/transport_sse.rs，`#[cfg(feature = "acp-sse")]`）

```rust
type ClientId = String;
type ClientRegistry = Arc<tokio::sync::Mutex<HashMap<ClientId, mpsc::Sender<SseEvent>>>>;

pub struct SseTransport {
    agent: Arc<AcpAgent>,          // 多 client 共享同一 AgentServer（axum State 注入）
    clients: ClientRegistry,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>, // request 关联
}
```

- `serve_sse(self, addr)` 内部把 `self` 包成 `Arc<AcpAgent>`，经 axum `State(Arc<AcpAgent>)` 注入每个连接 handler（clone 共享）。隐含 `AcpAgent` 需 `Send + Sync`（其字段 `server: AgentServer` 与 `mode` 应已满足；013 多连接 TCP server 已要求 `Send + Sync`）。
- 连接断开（SSE 流 close）→ `ClientRegistry` 移除对应 `client_id` → 后续不再推送（无泄漏）。

### 3. SSE 版 `AcpClient`（双工句柄）

```rust
struct SseAcpClient {
    tx: mpsc::Sender<SseEvent>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
}

impl AcpClient for SseAcpClient {
    async fn notify(&self, method: &str, params: Value) -> Result<(), AcpError>;
    //   序列化 → tx.send(SseEvent { event: method, data }) → SSE 输出
    async fn request(&self, method: &str, params: Value) -> Result<Value, AcpError>;
    //   分配 request_id → 注册 pending → tx.send(带 request_id 的 request event)
    //   → await oneshot（tokio::time::timeout 兜底）
}
```

- `request` 响应链路：agent → SSE(request event 带 id) → 客户端执行 → `POST /message`(带 id 的响应) → 服务器命中 pending → resolve oneshot。
- **超时**：`tokio::time::timeout` 兜底，`Elapsed` 映射为既有 `AcpError::Io`（`std::io::ErrorKind::TimedOut`），**不新增 `AcpError` 变体**（避免破坏下游 exhaustive match）。

### 4. permission mode per-session 修正（复用 014 时必做）

014 `AcpAgent.mode: Arc<RwLock<PermissionMode>>` 为**单全局**，单 client 下成立；多 client 下 `session/set_mode` 会跨 client 串扰。本任务修正为 **per-session**：

- `session/set_mode` 按 `sessionId` 记录（`HashMap<SessionId, PermissionMode>`，`Arc<RwLock<..>>` 包裹，或经 013 server 状态存储，Developer 定）。
- fs/权限判定按「当前处理的 session」查询对应 mode；默认 `default`。
- 加并发测试：两个 client 各自 `set_mode` 不同，互不串扰。

### 5. 复用与契约（勿改）

- 复用 014 `AcpAgent`（`handle` dispatch + 方法映射表 + `PermissionMode` + `AcpError` + `AcpClient` trait）、JSON-RPC 分帧 codec（stdio 侧）。
- 复用 013 `AgentServer` 做 session/lane 后端（session 隔离由 server 内部 `sessionId` 保证；多 client 共享同一 server 实例）。
- 复用 005/006 工具（工具装配仍下沉到 015 CLI/嵌入方，本任务不内置；fs 经 `AcpFsTool` 代理，014 已定义）。

### 6. feature 与依赖

```toml
[features]
acp-sse = ["dep:axum", "dep:tokio-stream"]   # 非 default（对齐 014「默认 feature 不含 acp-sse」）

[dependencies]
axum        = { version = "0.8", optional = true }   # HTTP server + SSE
tokio-stream = { version = "0.1", optional = true }  # mpsc → Stream（axum Sse 需要）

[dev-dependencies]
reqwest = { version = "0.12", features = ["stream"] }  # test-only SSE 客户端（不进入运行时依赖）
```

- `acp-sse` **非 default**：`axum`/`tokio-stream` 编译面较大，保持 default 精简；需要远程多 client 时显式 `--features acp-sse`。
- `reqwest` 仅 dev-dependency（集成测试起 SSE 客户端）；运行时**不引入 reqwest**（服务端无客户端 HTTP 需求）。若 Developer 想用 axum 依赖树中的 hyper 客户端替代 reqwest 做测试亦可，但不得新增运行时依赖。
- `--no-default-features` 下 `acp-sse` 关闭 → `axum`/`tokio-stream` 不链接；`--features acp-sse` 单独编译通过。

### 7. 边界声明（明确不做）

- **authenticate / TLS / 反向代理**：一期不做认证与加密（HTTP 明文，`authMethods: []`，014 已定）；生产安全由外部反向代理/TLS 终结。
- **HTTP 优雅停机 / 连接限流 / 背压策略**：一期用 `axum` 默认行为 + 有界 mpsc（channel 满则丢/报错，Developer 定并测试）。
- **SSE 断线重连 / 会话恢复协议**：客户端断线重连需重建 `client_id` 与事件流，一期不实现自动续流（session 本身经 `session/load` 可恢复，013 已支持）。
- **广播/多订阅同一 session**：一期每 client 独立 session 生命周期，不做多 client 共享同一 session 的实时广播。
- 不新增运行时依赖（除 `axum` + `tokio-stream`）；`reqwest` 仅 dev。

## Files

- src/acp/transport_sse.rs（`serve_sse` + axum router + `SseTransport`/`ClientRegistry`/`SseAcpClient` + pending map + 单测，`#[cfg(feature = "acp-sse")]`）
- src/acp/transport.rs（`serve_stdio` 保留不动；`serve_sse` 落地到 transport_sse 模块或在此 `#[cfg]` 实现，Developer 依据文件体量定）
- src/acp/mod.rs（`AcpAgent` 的 mode 改 per-session；`send + sync` 保证；`serve_sse` 声明）
- src/lib.rs（re-export；feature `acp-sse` 登记 axum/tokio-stream optional）
- Cargo.toml（`axum`/`tokio-stream` optional + `reqwest` dev-dep）
- tests/acp_sse.rs（SSE 集成测试，多 client）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets -D warnings passes
- [ ] cargo test --all-targets passes
- [ ] cargo fmt --check passes
- [ ] `cargo check --features acp-sse` 单独编译通过；`cargo test --no-default-features` 下 `axum`/`tokio-stream` 不链接、`acp-sse` 模块被门控跳过
- [ ] 单 client SSE 端到端（test 起 `serve_sse` + reqwest SSE 客户端）：initialize → session/new → session/prompt → 收到 SSE `session/update` 事件流 → 最终 `PromptResponse.stopReason` 正确
- [ ] 多 client 并发：两个 client 各建 session，事件流互不串扰、sessionId 唯一、prompt 各自独立完成
- [ ] `AcpClient::request` 双工：agent 发 `fs/read_text_file`（带 request_id）→ 客户端 POST 响应 → pending resolve 成功返回；超时走 `AcpError::Io`（TimedOut）
- [ ] per-session mode 隔离：两 client `session/set_mode` 不同，互不串扰
- [ ] 连接断开清理：client 断开 SSE → registry 移除 → 不再推送、无泄漏（断言 registry 大小归零）
- [ ] 产品代码无 `unwrap()`；异步测试用 `tokio::test`；单文件 ≤ 400 行，超则拆子模块并记录
- [ ] 新增运行时依赖仅 `axum` + `tokio-stream`（均 optional，feature `acp-sse`）；`reqwest` 仅 dev-dependency

## 修订记录

- v1.0（2026-09-06，Architect）：初稿。落地 014「SSE 为可选加分项」遗留。`serve_sse` 起 axum HTTP server（`GET /sse` 事件流 + `POST /message` JSON-RPC），wire 以 ACP v1 官方 SSE transport 为权威；多 client 经 `ClientRegistry`（`client_id → mpsc`）隔离，`AcpAgent` 包 `Arc` 经 axum `State` 共享；SSE 版 `AcpClient` 的 `notify` 走 mpsc、`request` 走 pending map + 超时（映射 `AcpError::Io` 不新增变体）；permission mode 由单全局修正为 per-session（多 client 串扰修复）；`axum`/`tokio-stream` feature-gated 在既有 `acp-sse`（非 default，PM 签核），`reqwest` 仅 dev-dep。复用 013/014 契约不动，边界排除 auth/TLS/断线重连/多 client 共享 session 广播。
