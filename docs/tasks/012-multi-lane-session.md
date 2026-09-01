# Task 012: 多 lane session（多 writer 并发 lane）

## Background

009 已交付单 writer 的 `SessionStorage` trait + `JsonlSessionStorage`（append-only JSONL + 崩溃恢复），但其边界声明明确「多 writer 并发 lane（多 agent 写同一 session 日志）属后续任务」；`SessionRecorder` 也只持单 lane 游标 `head`。

三期的「多 lane session」需要：同一 session 树（同一 JSONL 日志）可被多个 lane（多个 agent run / 分支）**并发写入**，每个 lane 有自己的写游标，fork 产生分支。这是 013 Agent Server（多 client / 多 session / 多 lane 调度）与 014 ACP（session 内并发 turn / 分支）的底座。

## Goal

- 定义 `SharedSessionStorage`：把「append 串行化」包一层，让多个 lane 并发写同一 `SessionStorage` 时 append 互斥、id 单调、落盘不交错
- 定义 `LaneWriter`：每个 lane 一个写游标（head），append 挂到 head 之后并推进；`fork_at` 从任意历史节点分支
- **不改** 009 已定稿的 `SessionStorage` trait 签名与 `JsonlSessionStorage`（append 语义保持 O(1) 追加、结构校验仍集中在 `load/reduce`）

## Design Notes

### 契约复用（勿改）

- `SessionStorage` trait（009 定稿）：`append(parent_id: Option<NodeId>, message: Message) -> Result<NodeId, SessionError>`、`load() -> Result<SessionTree, SessionError>`、`next_id() -> NodeId`
- `NodeId = u64`、`SessionTree`/`SessionNode`/`SessionEntry`、`reduce`、`SessionError`（009 定稿，勿改形状）
- `Message`（002 定稿）已 `Serialize + Deserialize`

### SharedSessionStorage（core/session.rs）

```rust
/// 进程内多 lane 共享的 session 写入口：串行化 append，委托 inner。
pub struct SharedSessionStorage {
    inner: Arc<dyn SessionStorage>,
    write_lock: tokio::sync::Mutex<()>,
}

impl SharedSessionStorage {
    pub fn new(inner: Arc<dyn SessionStorage>) -> Self;
    /// 取出内层 storage（load/next_id 透传，不串行）。
    pub fn inner(&self) -> &Arc<dyn SessionStorage>;
}

impl SessionStorage for SharedSessionStorage {
    async fn append(&self, parent: Option<NodeId>, msg: Message) -> Result<NodeId, SessionError> {
        let _guard = self.write_lock.lock().await;   // 串行化：锁全程持有，不跨其它 await
        self.inner.append(parent, msg).await
    }
    async fn load(&self) -> Result<SessionTree, SessionError> { self.inner.load().await }
    fn next_id(&self) -> NodeId { self.inner.next_id() }
}
```

- **串行化策略**：`tokio::sync::Mutex<()>` 只包 `append`；`load`/`next_id` 透传。`next_id` 底层已是 `AtomicU64`（009），锁内 `fetch_add` 保证 id 单调不重复。
- `SharedSessionStorage` 本身实现 `SessionStorage`，可放入任何需要 `Arc<dyn SessionStorage>` 的位置，零破坏。
- **边界**：`load` 与 `append` 并发不互斥——约定 `load` 只在「无活跃 lane 写」时调用（崩溃恢复入口），以 doc 注释声明，不做文件锁。

### LaneWriter（core/session.rs）

```rust
pub type LaneId = String;

/// 一个 lane 的写游标：head 指向树中本 lane 当前节点，append 挂到 head 之后并推进。
pub struct LaneWriter {
    storage: Arc<dyn SessionStorage>,   // 通常为 SharedSessionStorage
    lane_id: LaneId,
    head: Option<NodeId>,               // None = 尚无节点（初始空树）
}

impl LaneWriter {
    pub fn new(storage: Arc<dyn SessionStorage>, lane_id: impl Into<String>, head: Option<NodeId>) -> Self;
    pub fn lane_id(&self) -> &LaneId;
    pub fn head(&self) -> Option<NodeId>;

    /// 追加一条消息为 head 的子节点，推进 head，返回新节点 id。
    pub async fn append(&mut self, message: Message) -> Result<NodeId, SessionError> {
        let id = self.storage.append(self.head, message).await?;
        self.head = Some(id);
        Ok(id)
    }

    /// 从指定历史节点 fork：后续 append 挂到该节点之后（产生分支）。
    pub fn fork_at(&mut self, parent: Option<NodeId>) { self.head = parent; }
}
```

- `LaneWriter::append` 串行推进自身 `head`（`&mut self`，单 lane 内顺序写）；**多 lane 并发**由各自 `LaneWriter` + 共享 `SharedSessionStorage` 的 `write_lock` 保证 append 互斥。
- **分支语义**：两个 `LaneWriter` 从同一 `head` 各 `append` 一条 → 同一 parent 下两个子节点（`reduce` 已支持，009 单测覆盖）。
- 保留 009 `SessionRecorder` 不删（单 lane 便捷桥接仍可用）；`LaneWriter` 是通用化新类型，二者共存。

### 错误语义

- 复用 009 `SessionError`，不新增变体；`LaneWriter::append` 把 `SessionError` 原样上抛。

### 边界声明（明确不做）

- **仅进程内多 lane**：跨进程多写者（文件锁/flock）仍不在本任务（009/006 已声明）。
- **不保证 `load` 与并发 `append` 一致**：`load` 是恢复入口，须在 lane 停止后调用。
- **lane 调度/生命周期**（何时建 lane、何时 fork、lane 与 AgentHandle 绑定）属 013 Agent Server，不在本任务；本任务只提供「并发可写的共享存储 + 每 lane 游标」两原语。

## Files

- src/core/session.rs（`SharedSessionStorage` + `LaneWriter` + `LaneId` + 单测）
- src/core/mod.rs（登记导出，遵循既有惯例）
- src/lib.rs（re-export `SharedSessionStorage` / `LaneWriter` / `LaneId`）
- tests/session.rs（集成测试：多 lane 并发写 + fork 分支）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets -D warnings passes
- [ ] cargo test --all-targets passes
- [ ] cargo fmt --check passes
- [ ] `SharedSessionStorage` 单测/集成测试：多 task 并发 `append`（`join_all`）后 `load` 得到全部节点且 id 无重复、无交错半行；`load`/`next_id` 透传正确
- [ ] `LaneWriter` 单测：线性 `append` 推进 head；`fork_at` 后 append 落到 fork 分支；两 `LaneWriter` 同 head 各 append → `reduce` 出两叶子
- [ ] 与 009 `JsonlSessionStorage` 组合（tempdir）：`SharedSessionStorage::new(Arc::new(JsonlSessionStorage::open(...)))`，多 lane 并发 append → 崩溃恢复后树完整
- [ ] 产品代码无 `unwrap()`；异步测试用 `tokio::test`；tempdir 用 `tempfile`（若缺 dev-dep 则补并说明）
- [ ] 单文件 ≤ 400 行，超则拆子模块并记录

## 修订记录

- v1.0（2026-09-01，Architect）：初稿。在 009 单 writer 之上加 `SharedSessionStorage`（tokio Mutex 串行化 append，实现同一 `SessionStorage` trait，零破坏）+ `LaneWriter`（每 lane 一个 head 游标，fork = 换 head）；仅进程内多 lane，跨进程文件锁与 lane 调度均声明为后续（009/006/013）。
