# Task 013: Agent Server（多 client / 多 session / 多 lane 调度）

## Background

010 远程协议是「单 agent / 单连接」：一次 `serve` 对应一个 `AgentHandle`。三期需要**多 client**：一个 server 进程同时服务多个客户端连接，每个 client 可创建/加载多个 session，每个 session 可跑多个 lane（012）。同时需要一个**多 session 注册表**统一管理 session 生命周期与 lane 调度。

## Goal

- 定义 `SessionManager`：`session_id → SessionState`（存储 + 活跃 lane 集合）注册表
- 定义 `AgentServer` 门面：create/load/list session、spawn lane（绑定 `AgentHandle`）、prompt/continue/abort/fork、snapshot/subscribe
- 定义 session 协议 wire（在 010 哲学上扩展 session/lane 寻址）+ 多连接 TCP server（每连接一 task 并发 serve）
- 复用 001 `AgentHandle` / 003 `AgentRuntime` / 009 `SessionStorage` / 012 `SharedSessionStorage`+`LaneWriter`，**不改既有签名**

## Design Notes

### 契约复用（勿改）

- `AgentHandle`（001 定稿）：`snapshot() / subscribe() / wait_for_idle() / shutdown()` + `Agent` trait 方法
- `AgentRuntime`（003 定稿）：工具经 `AgentRuntime { tools: Vec<Arc<dyn Tool>> }` 注册，双参 spawn（004 已核验）
- `SessionStorage`（009）、`SharedSessionStorage`/`LaneWriter`/`LaneId`（012）
- 010 `RemoteError` 与 codec 帧思想（newline-delimited JSON，可复用 `src/remote/codec.rs`）

### 核心数据结构（src/server/mod.rs）

```rust
pub type SessionId = String;

pub struct AgentServer { inner: Arc<ServerInner> }

struct ServerInner {
    sessions: tokio::sync::Mutex<HashMap<SessionId, SessionState>>,
}

struct SessionState {
    session_id: SessionId,
    storage: Arc<dyn SessionStorage>,      // 通常为 SharedSessionStorage（012）
    lanes: HashMap<LaneId, LaneRuntime>,   // 活跃 lane
}

struct LaneRuntime {
    lane_id: LaneId,
    handle: AgentHandle,                   // 001：每个 lane = 一个活跃 runtime
}
```

### AgentServer 门面 API（src/server/mod.rs）

```rust
impl AgentServer {
    pub fn new() -> Self;

    /// 新建空 session；storage 为空树（session_id 已存在于注册表 → DuplicateSession）。
    pub async fn create_session(&self, session_id: SessionId, storage: Arc<dyn SessionStorage>)
        -> Result<(), ServerError>;

    /// 从持久化存储 load + reduce 重建 session 并注册（崩溃恢复入口）。
    pub async fn load_session(&self, session_id: SessionId, storage: Arc<dyn SessionStorage>)
        -> Result<(), ServerError>;

    pub async fn list_sessions(&self) -> Vec<SessionId>;

    /// 在 session 内 spawn 一个 lane：spawn AgentRuntime → 得到 AgentHandle → 登记。
    /// 可选：挂 LaneWriter 桥接 001 subscribe() 持久化（复用 012 + 009 SessionRecorder 思想）。
    pub async fn spawn_lane(&self, session_id: &str, lane_id: &str, runtime: AgentRuntime)
        -> Result<(), ServerError>;

    pub async fn prompt(&self, session_id: &str, lane_id: &str, messages: Vec<Message>)
        -> Result<(), ServerError>;
    pub async fn continue_(&self, session_id: &str, lane_id: &str) -> Result<(), ServerError>;
    pub async fn abort(&self, session_id: &str, lane_id: &str) -> Result<(), ServerError>;
    pub async fn reset(&self, session_id: &str, lane_id: &str) -> Result<(), ServerError>;

    /// 从 from_lane 的当前 head 分支出新 lane（新 runtime），后续写落到新分支。
    pub async fn fork_lane(&self, session_id: &str, from_lane: &str, new_lane: &str, runtime: AgentRuntime)
        -> Result<(), ServerError>;

    pub fn snapshot(&self, session_id: &str, lane_id: &str) -> Option<AgentSnapshot>;
    pub fn subscribe(&self, session_id: &str, lane_id: &str) -> Option<broadcast::Receiver<AgentEvent>>;
    pub async fn shutdown(&self) -> Result<(), ServerError>;
}
```

- **并发策略**：注册表用 `tokio::sync::Mutex`（写多、粒度粗、保简单）；不跨 await 持锁——`spawn_lane` 先 spawn 再入表，避免把 `runtime.spawn` 的 await 放进锁内。
- **lane 语义**：一个 lane = 一个活跃 `AgentHandle`（一个 runtime task）。`fork_lane` 从某 lane 分支 = 建新 runtime，其 transcript 种子取自 session 树 `path_to`（009）；持久化写入经新 lane 的 `LaneWriter`（012），`fork_at` 到源 lane 的 head。
- `snapshot/subscribe` 返回 `Option`：session/lane 不存在返回 `None`（不 panic）。

### Session 协议 wire（src/server/protocol.rs）

沿用 010 的 newline-delimited JSON 帧（复用 codec），命令面扩展 session/lane 寻址：

```rust
/// 客户端 → 服务端
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerRequest {
    CreateSession { id: u64, session_id: Option<SessionId> }, // None = 服务端分配
    LoadSession   { id: u64, session_id: SessionId },
    ListSessions  { id: u64 },
    SpawnLane     { id: u64, session_id: SessionId, lane_id: LaneId },
    ForkLane      { id: u64, session_id: SessionId, from_lane: LaneId, new_lane: LaneId },
    Prompt        { id: u64, session_id: SessionId, lane_id: LaneId, messages: Vec<Message> },
    Continue      { id: u64, session_id: SessionId, lane_id: LaneId },
    Abort         { id: u64, session_id: SessionId, lane_id: LaneId },
    Reset         { id: u64, session_id: SessionId, lane_id: LaneId },
    GetSnapshot   { id: u64, session_id: SessionId, lane_id: LaneId },
    Subscribe     { id: u64, session_id: SessionId, lane_id: LaneId },
    Unsubscribe   { id: u64, session_id: SessionId, lane_id: LaneId },
    Shutdown      { id: u64 },
}

/// 服务端 → 客户端
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Response    { id: u64, result: Result<serde_json::Value, String> },
    SessionList { id: u64, sessions: Vec<SessionId> },
    Snapshot    { session_id: SessionId, lane_id: LaneId, snapshot: AgentSnapshot },
    Event       { session_id: SessionId, lane_id: LaneId, event: AgentEvent },
}
```

- `id` 单调递增由客户端分配，应答同 id（同 010）；`Response.result` 为 `Result<serde_json::Value, String>`（Ok 负载因方法而异，如 CreateSession 返回分配的 session_id）。
- `Subscribe` 后，服务端把该 lane 的 `AgentEvent` 包成 `Event` 推送（带上 session/lane 前缀，客户端据此路由）。

### 多连接 TCP server（src/server/transport.rs）

```rust
impl AgentServer {
    /// 监听 TCP，accept 循环；每连接 spawn 一个 task 跑 serve_connection。
    pub async fn serve_tcp(self: Arc<Self>, addr: std::net::SocketAddr) -> Result<(), ServerError>;
    /// 服务一条连接：读 ServerRequest → 分发到 AgentServer 方法；订阅事件转发。
    pub async fn serve_connection<S>(self: Arc<Self>, stream: S) -> Result<(), ServerError>
    where S: AsyncRead + AsyncWrite + Send + Unpin + 'static;
}
```

- 复用 010 codec（newline-delimited JSON）+ 写半单 writer task 模式（不持锁跨 await）。
- 连接关闭只移除该连接的订阅，不关 session/lane（多 client 共享 session 语义）。

### 错误语义（src/server/mod.rs）

```rust
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("session not found: {0}")]     SessionNotFound(SessionId),
    #[error("lane not found: {0}")]        LaneNotFound(LaneId),
    #[error("lane already exists: {0}")]   LaneAlreadyExists(LaneId),
    #[error("duplicate session: {0}")]     DuplicateSession(SessionId),
    #[error("session error: {0}")]         Session(#[from] SessionError),
    #[error("agent error: {0}")]           Agent(String),   // AgentError 字符串化
    #[error("io error: {0}")]              Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]        Protocol(String),
}
```

### 边界声明（明确不做）

- **认证/TLS/鉴权**：不做（同 010 边界）；transport 安全由上层解决。
- **跨进程多写者**（文件锁）：不在本任务（009/012 已声明）；多 client 在同一 server 进程内共享 session。
- **ACP**：不在本任务，属 014；本任务只提供 transport 无关核心 + 原生 TCP 多连接协议。
- **lane 的模型/工具差异**：所有 lane 用同一 `AgentRuntime` 装配（spawn_lane 传入 runtime）；per-lane 差异化配置属后续。

## Files

- src/server/mod.rs（`AgentServer`/`SessionState`/`LaneRuntime`/`ServerError` + 单测）
- src/server/protocol.rs（`ServerRequest`/`ServerMessage` + 单测）
- src/server/transport.rs（`serve_tcp`/`serve_connection` + 单测）
- src/core/mod.rs / src/lib.rs（登记 `pub mod server` + re-export `AgentServer`/`ServerError`/协议类型）
- tests/server.rs（集成测试，duplex/loopback + 多 client 并发）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets -D warnings passes
- [ ] cargo test --all-targets passes
- [ ] cargo fmt --check passes
- [ ] `AgentServer` 单测：create/list/load session；spawn_lane 后 prompt 路由正确；snapshot/subscribe 返回对应 lane 状态；不存在的 session/lane 返回 `None`/错误不 panic
- [ ] `fork_lane`：从源 lane 分支后，新 lane 写落新分支（配合 012 `LaneWriter` 断言 `reduce` 出两叶子）
- [ ] 协议单测：`ServerRequest`/`ServerMessage` 全变体序列化 roundtrip（含 Response Ok/Err）
- [ ] 多连接测试（duplex 或 loopback tcp）：两 client 并发连同一 `AgentServer`，各自 create session + spawn lane + prompt，事件各归各（session/lane 前缀路由正确）；一 client 断开不影响另一 client 的 session
- [ ] 产品代码无 `unwrap()`；异步测试用 `tokio::test`；不依赖真实网络（duplex / `TcpListener` bind 127.0.0.1:0）
- [ ] 单文件 ≤ 400 行，超则拆子模块并记录

## 修订记录

- v1.0（2026-09-01，Architect）：初稿。多 session 注册表 + 多 lane 调度核心（transport 无关）+ 原生多连接 TCP 协议（复用 010 codec 帧）；每 lane = 一个活跃 AgentHandle；fork_lane 复用 012 LaneWriter 分支；010 协议保持单连接不扩展，多 client 走本任务或 014 ACP。
