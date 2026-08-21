# Task 001: Agent trait + 生命周期（AgentHandle）

## Background

guigu 核心原则是 Trait-based 抽象：Agent、Tool、Runtime 均为 trait。并发模型定稿为**单 writer**：对外 `AgentHandle`（Clone）→ `mpsc<Command>` → 唯一 runtime task → `watch<AgentSnapshot>`（权威）+ `broadcast<AgentEvent>`（增量）。本任务交付 Agent 的 trait 契约与 actor 外壳骨架，主循环本体在 003。

## Goal

- 定义 `Agent` trait（行为契约）
- 实现 `AgentHandle`：命令队列、snapshot、订阅、abort、wait_for_idle 的骨架
- 提供**最小内存实现**（不接 LLM）：prompt 直接追加 transcript 到 snapshot，验证生命周期全通路
- 不采用 `Arc<RwLock<AgentState>>` 作为核心设计；状态归唯一 runtime task 所有

## Design Notes

### Agent trait

```rust
// src/core/agent.rs
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

### AgentCommand / AgentHandle

```rust
pub enum AgentCommand {
    Prompt(Vec<Message>),
    Continue,
    Steer(Message),
    FollowUp(Message),
    Abort,
    Reset,
    Shutdown,
}

#[derive(Clone)]
pub struct AgentHandle {
    tx: mpsc::Sender<AgentCommand>,
    snapshot: watch::Receiver<AgentSnapshot>,
    events: broadcast::Receiver<AgentEvent>,
    idle: Arc<Notify>,           // wait_for_idle 用
}

impl AgentHandle {
    pub fn spawn(config: AgentConfig) -> Self;          // 启动唯一 runtime task
    pub fn snapshot(&self) -> AgentSnapshot;            // watch 最新
    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent>;
    pub async fn wait_for_idle(&self) -> Result<(), AgentError>;
    pub async fn shutdown(self) -> Result<(), AgentError>;  // 发 Shutdown，等 task 退出
}

impl Agent for AgentHandle { /* 转发到 tx */ }
```

### AgentSnapshot（对外不可变）

```rust
pub struct AgentSnapshot {
    pub system_prompt: String,
    pub model: ModelId,
    pub thinking_level: ThinkingLevel,
    pub messages: Vec<Arc<Message>>,      // 完整 transcript 的不可变视图
    pub is_streaming: bool,
    pub streaming_message: Option<Arc<Message>>,
    pub pending_tool_calls: HashSet<String>,
    pub error_message: Option<String>,
}
```

- `watch` 只传 `AgentSnapshot`（权威最新状态）；`broadcast` 只传 `AgentEvent`（瞬态增量）。
- 订阅者 broadcast lag 时须重读 snapshot，**不把 broadcast 当审计/持久化通道**（文档注明，代码用 `lagged()` 重置即可）。
- 主循环与工具执行期间**不持锁跨 await**；snapshot 更新用 `watch::Sender::send` 整体替换，不提供可变借用。

### 最小内存实现（本任务范围）

- `InMemoryAgent`（或 `AgentHandle` 的默认 runtime task 第一版）：`Prompt` 收到后直接把 user 消息 append 到内部 transcript，依次发 `MessageStart/MessageEnd/TurnEnd/AgentEnd`，更新 snapshot。
- 不做 LLM、不做工具执行（003 接入）。
- 目的：让生命周期（订阅、steer/followUp 队列、abort、wait_for_idle）有真实逻辑可测，杜绝 fake green。

### 行为契约（本任务内）

- 并发 `prompt` 在 active run 期间应排队或返回错误（`AgentError::Busy`），实现者二选一并记录。
- `abort()` 不阻塞调用方；被取消的 run 最终产出 `stop_reason: Aborted` 的 assistant 消息（003 强化）。
- `wait_for_idle` 在 `AgentEnd` 的所有 listener 结算后返回。

## Files

- src/core/agent.rs
- src/core/event.rs（复用 002）
- src/core/mod.rs
- tests/agent_lifecycle.rs

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy -D warnings passes
- [ ] cargo test passes
- [ ] cargo fmt --check passes
- [ ] tests/agent_lifecycle.rs 覆盖：prompt 后 snapshot.messages 增长；subscribe 收到完整事件序列；steer/followUp 入队与 drain；abort 后 run 结束且状态一致；wait_for_idle 正确结算；reset 清空 transcript 与队列
- [ ] 无 `unwrap()`；异步测试用 `tokio::test` 真实执行
- [ ] 单文件超 400 行时按 responsibility 拆子模块并记录
