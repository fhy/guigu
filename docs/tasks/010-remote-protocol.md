# Task 010: 远程协议（Remote Protocol）

## Background

001/003 已交付进程内 `AgentHandle`（mpsc 命令队列 + watch 快照 + broadcast 事件），但对外接口是进程内类型：客户端必须与 runtime 同进程。guigu 的目标是「Embeddable + 可独立运行」——需要一个**跨进程**的远程协议，把 `AgentHandle` 的命令面与事件流序列化到字节流上，让「客户端进程」与「agent 运行进程」解耦（如 CLI/TUI 驱动独立 server 进程）。

architecture 6 二期明确「插件与远程协议」；009 的边界声明把「多 writer 并发 lane（多 agent 写同一 session 日志）」归到「010 远程协议 / 后续任务」。本任务只做**单 agent 的远程驱动协议**，多 lane 并发写 session 明确**不在本任务**（见边界声明）。

## Goal

- 定义 transport 无关的 wire 协议：serde 序列化 + **newline-delimited JSON**（一行一条消息）帧
- 实现 `RemoteServer`：把 `AgentHandle` 的命令面 + 事件流暴露到字节流
- 实现 `RemoteClient`：跨进程的 handle 等价物（同名命令方法 + 事件订阅 + 快照）
- 提供两种 connector：`stdio`（子进程）与 `tcp`（`tokio::net`），复用同一 codec
- 用 `tokio::io::duplex` loopback 测试全链路，不依赖真实网络

## Design Notes

### 复用既有契约（勿改签名）

- `AgentHandle`（001 定稿）：`snapshot() -> AgentSnapshot`、`subscribe() -> broadcast::Receiver<AgentEvent>`、`wait_for_idle()`、`shutdown()`，及 `Agent` trait 方法 `prompt / continue_ / steer / follow_up / abort`。
- `AgentCommand`（001 定稿）：`Prompt(Vec<Message>) / Continue / Steer(Message) / FollowUp(Message) / Abort / Reset / Shutdown`。远程命令面与之**一一对应**，不新增语义。
- `Message`（002 定稿）已 `Serialize + Deserialize`；`AgentEvent` 002 已要求序列化 roundtrip。**本任务要求 `AgentSnapshot`、`AgentEvent` 及其依赖（`AssistantEvent`、`ModelId`、`ThinkingLevel`、`ToolResult`）均 `derive(Serialize, Deserialize)`**；若当前缺失，由 Developer 补齐（纯 `derive`，不改形状/语义），并沿用 serde `rc` feature 序列化 `Arc<Message>`（002 已启用）。
- 事件语义与进程内一致：`broadcast` 只传增量；远端订阅者 lag 时重读 snapshot（协议天然支持：连接时先发一份 snapshot，之后按需 `GetSnapshot`）。

### Wire 协议（src/remote/protocol.rs）

```rust
/// 客户端 → 服务端
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RemoteRequest {
    Prompt     { id: u64, messages: Vec<Message> },
    Continue   { id: u64 },
    Steer      { id: u64, message: Message },
    FollowUp   { id: u64, message: Message },
    Abort      { id: u64 },
    Reset      { id: u64 },
    GetSnapshot{ id: u64 },
    Shutdown   { id: u64 },
}

/// 服务端 → 客户端
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// 对 GetSnapshot 的应答
    Snapshot { id: u64, snapshot: AgentSnapshot },
    /// 对命令（Prompt/Continue/Steer/FollowUp/Abort/Reset/Shutdown）的应答
    Response { id: u64, result: Result<(), String> },
    /// 服务端推送的 agent 事件
    Event { event: AgentEvent },
}
```

- `id` 为请求关联号，由客户端单调递增（`AtomicU64`），应答携带同 `id`。
- `Response.result` 为 `Result<(), String>`：`Ok(())` 表示命令被 runtime **接受**（与进程内一致，`prompt` 入队即返回，不等待 run 结束——等待 run 结束用 001 的 `wait_for_idle` 语义，远程侧由客户端自行以事件/快照判定）；`Err(String)` 为 `AgentError` 的字符串化。
- 连接建立后，服务端**立即推送一条初始 `Snapshot`**（`id = 0`，或约定一个保留 id），作为客户端基线（对齐「lag → 重读 snapshot」契约）。

### 帧协议（src/remote/codec.rs）

- 每条消息序列化为**单行 JSON**，以 `\n` 结尾；反序列化按行 `read_line`。
- 空行忽略；某行 `serde_json` 解析失败 → 返回 `RemoteError::Protocol`（协议层错误，与 009 的 JSONL「尾部半行跳过」不同：本协议是**在线双向流**，不是 append-only 日志，任何坏帧都属协议违规）。
- 对 `AsyncRead + AsyncWrite + Send + Unpin` 的泛型字节流工作；写侧用 `tokio::io::split` 拆分读写半，**写半归单一 writer task**（`mpsc` 汇入 → 写循环），避免多任务持锁跨 await（遵守「不持锁跨 await」原则）。

### RemoteServer（src/remote/server.rs）

```rust
pub struct RemoteServer { handle: AgentHandle }

impl RemoteServer {
    pub fn new(handle: AgentHandle) -> Self;
    /// 服务一条连接：先推初始 Snapshot，再读 RemoteRequest 分发，事件订阅转发。
    pub async fn serve<S>(&self, stream: S) -> Result<(), RemoteError>
    where S: AsyncRead + AsyncWrite + Send + Unpin + 'static;
}
```

- `serve` 内部：
  1. `tokio::io::split(stream)` → 读半 + 写半。
  2. 写半包进 `mpsc::UnboundedSender<ServerMessage>` + 单写 task（`write_all(line)` 后 `flush`）。
  3. 订阅 `handle.subscribe()` → 单独 task 把每条 `AgentEvent` 包成 `Event` 写入 mpsc（`recv` 循环）。
  4. 先发初始 `Snapshot`，再进入读循环逐条解析 `RemoteRequest` 分发：
     - `Prompt/Continue/Steer/FollowUp/Abort/Reset` → 调 `handle` 对应方法（`abort` 为同步入队），回 `Response{id, result}`。
     - `GetSnapshot` → 回 `Snapshot{id, snapshot: handle.snapshot()}`。
     - `Shutdown` → 回 `Response{id, Ok}` 后 `handle.shutdown().await`，关闭写半退出。
  5. 读半 EOF / 连接关闭 → 正常返回（不发 Shutdown 到 handle，除非收到 Shutdown 请求）。

### RemoteClient（src/remote/client.rs）

```rust
pub struct RemoteClient {
    tx: mpsc::UnboundedSender<RemoteRequest>,   // 写 task 的入参
    snapshot: watch::Receiver<AgentSnapshot>,   // 本地缓存，watch 更新
    events: broadcast::Sender<AgentEvent>,      // 本地重建事件源
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<(), String>>>>>,
    next_id: AtomicU64,
}

impl RemoteClient {
    /// 连接既有字节流（stdio/tcp 由 connector 提供）。
    pub async fn connect<S>(stream: S) -> Result<Self, RemoteError>
    where S: AsyncRead + AsyncWrite + Send + Unpin + 'static;

    // 与 AgentHandle/Agent trait 同名的命令面（返回 Result<(), RemoteError>）：
    pub async fn prompt(&self, messages: Vec<Message>) -> Result<(), RemoteError>;
    pub async fn continue_(&self) -> Result<(), RemoteError>;
    pub async fn steer(&self, message: Message) -> Result<(), RemoteError>;
    pub async fn follow_up(&self, message: Message) -> Result<(), RemoteError>;
    pub fn abort(&self) -> Result<(), RemoteError>;       // 入队即返回，不 await 应答
    pub async fn reset(&self) -> Result<(), RemoteError>;
    pub async fn shutdown(self) -> Result<(), RemoteError>;

    pub fn snapshot(&self) -> AgentSnapshot;              // 本地 watch 最新
    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent>;  // 本地重建事件源
}
```

- `connect` 内部：split 流；启动**单一读 task**，循环解析 `ServerMessage`：
  - `Response{id, result}` → 从 `pending` 取出 oneshot 发送结果；
  - `Snapshot{..}` → `watch::Sender::send` 更新本地快照；
  - `Event{event}` → 写入本地 `broadcast`。
  - 读 task 结束（EOF）→ 向 `watch` 发错误态/标记关闭（实现者定，至少让后续命令返回 `RemoteError`）。
- `prompt` 等命令：分配 `id`、插 oneshot 入 `pending`、`tx.send(RemoteRequest::...)`、`await` oneshot（超时兜底，默认 30s，超时返回 `RemoteError::Timeout` 并从 `pending` 移除）。
- `abort`：`tx.send` 后立即返回 `Ok`，不等待应答（与 001 进程内 `abort` 非阻塞一致）。
- `snapshot()` 返回本地缓存（初始由连接时 snapshot 填充，之后由服务端 `Snapshot` 推送更新）。

### Connector（src/remote/mod.rs 内，或独立小函数）

- `stdio`：客户端侧以 `tokio::process::Command` 启动 agent 子进程，取 stdin/stdout 组合为双工流（可用 `tokio::io::duplex` 桥接，或直接 `AsyncRead`/`AsyncWrite` 各半）。服务端侧直接对 `tokio::io::stdin()/stdout()` 跑 `serve`。
- `tcp`：`tokio::net::TcpListener` + `TcpStream`（`TcpStream` 天然实现 `AsyncRead + AsyncWrite`）。
- connector 只负责「产出满足 codec 泛型约束的字节流」，协议/codec 层不感知具体 transport。

### 错误语义（src/remote/protocol.rs）

```rust
#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialize error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("command error: {0}")]
    Command(String),       // 服务端 AgentError 字符串化
    #[error("request timeout")]
    Timeout,
}
```

### 边界声明（明确不做）

- **单 agent / 单 server**：一次 `serve` 对应一个 `AgentHandle`；多 client 并发连同一 server、多 agent 写同一 session 日志（多 lane）**不在本任务**（属后续，009 已声明）。
- **认证 / TLS / 鉴权**：不做。transport 安全由上层或 connector 扩展解决。
- **跨进程同文件写串行化**（flock）：不在本任务（006 已声明）。
- **远程侧 `wait_for_idle` 等价语义**：不单独造轮子，客户端以「收到 `AgentEnd` 事件」或「`Snapshot` 中 `is_streaming == false`」判定 run 结束（对齐 001 的同步点哲学）。
- **`impl Agent for RemoteClient`**：本任务不强制（broadcast 类型耦合 tokio），仅提供同名命令面；是否后续统一由实现者评估记录。

## Files

- src/remote/mod.rs（登记子模块 + connector 辅助 + re-export）
- src/remote/protocol.rs（RemoteRequest / ServerMessage / RemoteError + 单测）
- src/remote/codec.rs（newline-delimited JSON 帧 + 单测）
- src/remote/server.rs（RemoteServer + 单测）
- src/remote/client.rs（RemoteClient + 单测）
- src/core/mod.rs / src/lib.rs（登记 `pub mod remote` + 按既有 facade 惯例 re-export `RemoteServer` / `RemoteClient` / `RemoteError` / 协议类型）
- tests/remote.rs（集成测试，duplex loopback 驱动）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets -D warnings passes
- [ ] cargo test --all-targets passes
- [ ] cargo fmt --check passes
- [ ] codec 单测：单行 roundtrip；多行合并读；一行拆半写（partial line）；空行忽略；坏 JSON → `Protocol` 错误
- [ ] protocol 单测：`RemoteRequest`/`ServerMessage` 全变体序列化 roundtrip（含 `Response` 的 Ok/Err 两态）
- [ ] server 单测（duplex）：连接后收到初始 Snapshot；Prompt 后收到 `Event`（`AgentStart→...→AgentEnd` 序列，用 fake 驱动的最小 `AgentHandle` 或真实 `AgentHandle` + 001 内存实现）；GetSnapshot 返回当前快照；Shutdown 后流关闭
- [ ] client 单测（duplex + 对端 server）：`prompt` 返回 Ok；`subscribe()` 收到事件；`snapshot()` 随服务端推送更新；命令错误路径回传为 `Command` 错误
- [ ] 集成测试（tests/remote.rs）：loopback 端到端——client `prompt` → server 驱动 runtime → client 收到完整事件序列与更新后的 snapshot；`abort` 非阻塞；`reset` 后 snapshot.messages 清空
- [ ] 产品代码无 `unwrap()`；异步测试用 `tokio::test` 真实执行；`tokio::io::duplex` 做 loopback，不依赖真实网络
- [ ] 单文件 ≤ 400 行，超则拆子模块并记录
- [ ] 无新增依赖（tokio `full` 已含 `net`/`process`/`io`，serde_json 已有）

## 修订记录

- v1.0（2026-08-31，Architect）：初稿。协议 = serde + newline-delimited JSON（复用 009 的 JSONL 帧思想，但语义为在线双向流而非 append-only 日志）；命令面与 001 `AgentCommand` 一一对应；复用 002 已序列化的 `Message`/`AgentEvent`，要求补齐 `AgentSnapshot` 等序列化 derive；`RemoteClient` 用 watch+broadcast 本地重建进程内契约；连接即推初始快照对齐「lag→重读 snapshot」；单 agent 边界，多 lane 并发写 session 明确排除（后续任务）；无新增依赖。
