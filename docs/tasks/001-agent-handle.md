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
    idle: Arc<Notify>,           // wait_for_idle 用（v1.1：建议改为 watch<bool>，见行为契约）
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

### 行为契约（本任务内，v1.1 已定稿）

**并发 prompt：active run 期间排队。**
- 选定方案：`active run` 期间的 `prompt` 不返回 Busy，而是与 steer/followUp 一起进入
  同一 FIFO 命令队列，待当前 run 结束后按序处理（命令本身已由 `mpsc` 天然排队）。
- 实现者在 `AgentCommand::Prompt` 的文档注释中记录该选择；`AgentError::Busy` 仍保留
  为类型成员，但本任务内不使用（003 若引入超时/丢弃语义再启用）。
- 测试须覆盖"排队执行"语义，禁止把"无并发检查"写成预期。

**事件序列（固定，与 pi 对齐）。**
- 单个 run（一次 Prompt/Continue 处理）的事件序列固定为：
  `TurnStart → (逐消息 MessageStart → MessageEnd) → TurnEnd → AgentEnd`。
- 多消息 prompt 时，`MessageStart/MessageEnd` 按消息逐条包裹，即
  `M1S → M1E → M2S → M2E`（不是 M1S → M2S → M1E → M2E）。
- 订阅测试必须完整断言该序列（含 `AgentStart` 开头），不得只断言"收到过某事件"。

**wait_for_idle：以 `AgentEnd` 为同步点，含超时兜底。**
- `wait_for_idle` 在 `AgentEnd` 发出且所有 listener 结算后返回；同一 run 结束可多次调用，
  不能因"通知已发出"而永久挂起（r6 死锁根因）。
- 实现建议：放弃裸 `Notify`，改用 `watch<bool>`（idle 标志，AgentEnd 后置 true）+
  循环 `changed().await` 直到为真；或先查状态快照再注册等待 + `notify_one` 补发。
- **必须**有超时兜底（默认 5s），防止任何路径下的永久挂起。
- 测试**禁止**用 `tokio::time::sleep()` 做同步点（r6 flaky 根因），一律以
  `wait_for_idle` 或收到 `AgentEnd` 为同步点。

**reset：清空 transcript 与命令队列。**
- `Reset` 处理时清空内部 transcript、丢弃队列中未处理的 Prompt/Steer/FollowUp，
  并重置 `is_streaming / streaming_message / pending_tool_calls / error_message`。
- Reset 完成后 idle 标志为真，`wait_for_idle` 可立即返回；snapshot 中的
  system_prompt / model / thinking_level 保持不变。

**abort：不阻塞调用方，run 结束且状态一致。**
- `abort()` 只入队即返回（try_send），不做 await。
- 被取消的 run 产出 `stop_reason: Aborted` 的 assistant 消息并照常发出
  `MessageEnd/TurnEnd/AgentEnd`（003 强化，本任务需保证 AgentEnd 必达）。
- abort 后 `wait_for_idle` 正常返回，测试不得依赖调度时序（r6 flaky）。

**shutdown：等待 runtime task 真正退出。**
- `shutdown(self)` 发送 `Shutdown` 后，必须 `await` 保存的 `JoinHandle` 直至 task 退出
  再返回 Ok（r6 第 8 条：不能 send 即返回）。

## Files

- src/core/agent.rs
- src/core/event.rs（复用 002）
- src/core/mod.rs
- tests/agent_lifecycle.rs

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets -D warnings passes
- [ ] cargo test passes（无挂起，`--all-targets` 覆盖集成测试）
- [ ] cargo fmt --check passes
- [ ] tests/agent_lifecycle.rs 覆盖：
  - prompt 后 snapshot.messages 增长
  - subscribe 收到完整事件序列 `AgentStart→TurnStart→M1S→M1E→M2S→M2E→TurnEnd→AgentEnd`（多消息）
  - steer/followUp 入队与 drain
  - 并发 prompt 排队执行（active run 期间第二条 prompt 之后才被处理）
  - abort 后 run 结束且状态一致（AgentEnd 必达，wait_for_idle 正常返回）
  - wait_for_idle 正确结算（多次调用、同 run 内、Reset 后均可返回）
  - reset 清空 transcript 与队列
- [ ] 测试以 `wait_for_idle` / subscribe 收 `AgentEnd` 为同步点，无 `tokio::time::sleep` 竞态
- [ ] 产品代码无 `unwrap()`；测试内用 `expect("前置条件说明")` 替代裸 `unwrap()`
- [ ] 异步测试用 `tokio::test` 真实执行
- [ ] 单文件超 400 行时按 responsibility 拆子模块并记录

## 修订记录

- v1.1（2026-08-25，Architect）：依据 r6 审查（docs/reviews/001-review-r6.md）定稿行为契约。
  - 并发 prompt 选定"active run 期间排队"（r6 #6）
  - 事件序列固定为逐消息包裹 M1S→M1E→M2S→M2E（r6 #7）
  - wait_for_idle 改为 AgentEnd 同步点 + 超时兜底，禁止 sleep 竞态（r6 #1/#3/#9）
  - reset 契约明确为清空 transcript 与队列（r6 #4/#5）
  - shutdown 需等待 JoinHandle（r6 #8）
  - 验收标准增加 `--all-targets`、事件序列断言、expect 替代 unwrap（r6 #2/#10）
