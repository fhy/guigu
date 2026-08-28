# Task 009: Session 树 + JSONL 崩溃恢复

## Background

001/003 已交付单 writer 运行时，transcript 是纯内存 `Vec<Arc<Message>>`，进程退出即丢失。architecture 3.8 明确：一期 `InMemorySessionStorage`，二期落地 **Session 树 / 分支 fork / JSONL 文件后端 / 崩溃恢复（reducer）**，并预留 `SessionStorage` trait 接口。

本任务落地二期持久化：把会话历史以 **append-only JSONL** 落盘，支持**树形分支（fork）**，进程崩溃后由 **reducer 重放** 重建会话树并恢复「继续 append 不重复 id」的能力。

## Goal

- 定义 Session 树结构（`SessionTree` / `SessionNode` / `SessionEntry`），支持分支 fork
- 落定 `SessionStorage` trait（architecture 3.8 预留接口）
- 实现 `JsonlSessionStorage`：append-only JSONL 持久化 + 崩溃恢复
- 纯函数 `reduce` 重放 entries 重建树（含结构校验）
- 全链路**脱离网络、tempdir 单测**；提供可选 `SessionRecorder` 桥接演示端到端

## Design Notes

### 契约复用（以既有定稿为准，勿改）

- `Message` / `UserMessage` / `AssistantMessage` / `ToolResultMessage` 均 002 定稿，`#[derive(Serialize, Deserialize)]` 且用 `#[serde(tag = "type", rename_all = "snake_case")]`，**可直接序列化进 JSONL**，009 不改其形状。
- `session_id` 为 `String`（与 007 `ProviderRequest.session_id: Option<String>` 一致，不引入 newtype）。

### 数据结构（core/session.rs）

```rust
pub type NodeId = u64;

/// 会话树（reducer 产物，内存态）
pub struct SessionTree {
    pub session_id: String,
    pub root: Option<NodeId>,              // 根节点 id（单根）
    pub nodes: BTreeMap<NodeId, SessionNode>,
}

pub struct SessionNode {
    pub id: NodeId,
    pub parent_id: Option<NodeId>,         // None = 根
    pub message: Message,                  // 本节点承载的消息
    pub children: Vec<NodeId>,             // 直接子节点（reducer 填充，冗余便于遍历）
}

impl SessionTree {
    /// 叶子节点集合（children 为空 = 各活跃分支头）
    pub fn leaves(&self) -> Vec<NodeId>;
    /// 从根到某叶的线性消息序列（用于恢复 transcript）
    pub fn path_to(&self, leaf: NodeId) -> Option<Vec<&Message>>;
}

/// JSONL 一行（append 的序列化单元）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEntry {
    pub id: NodeId,
    pub parent_id: Option<NodeId>,         // fork：指向任意历史节点 id
    pub message: Message,
}
```

### SessionStorage trait（core/session.rs，落定预留接口）

```rust
#[async_trait]
pub trait SessionStorage: Send + Sync {
    /// 追加一条消息为新节点，返回新节点 id。
    /// O(1) append-only：不校验 parent 是否存在（结构校验集中在 load/reduce）。
    async fn append(&self, parent_id: Option<NodeId>, message: Message)
        -> Result<NodeId, SessionError>;

    /// 读全量 entries + reduce 重建树（崩溃恢复入口）。
    async fn load(&self) -> Result<SessionTree, SessionError>;

    /// 当前已分配的最大 id（崩溃恢复后必须恢复到 max(id)+1，保证续写不重复）。
    fn next_id(&self) -> NodeId;
}
```

- 若 `src/core/session.rs` 已有一期 `InMemorySessionStorage`，Developer 以本规格为权威对齐/重构；若尚未实现，按本规格新建。是否保留 `InMemorySessionStorage` 由实现者定，但**不得**与 `SessionStorage` trait 签名冲突。

### JsonlSessionStorage（core/session.rs）

```rust
pub struct JsonlSessionStorage {
    path: PathBuf,
    session_id: String,
    next_id: AtomicU64,
}

impl JsonlSessionStorage {
    /// 打开（文件不存在则创建）；读全量恢复 next_id。失败返回 SessionError。
    pub async fn open(path: impl Into<PathBuf>, session_id: impl Into<String>)
        -> Result<Self, SessionError>;
}
```

**append 流程**（保证崩溃恢复语义）：

1. `id = self.next_id.fetch_add(1, SeqCst)`
2. 构造 `SessionEntry { id, parent_id, message }` → `serde_json::to_string`
3. `OpenOptions::new().create(true).append(true).open(path)` → `write_all(line + "\n")` → `sync_all()`
4. 返回 `id`

- **单行原子性**：`O_APPEND` 下单次 `write_all` 一行是原子的（单 writer，无并发交叉）；`sync_all()` 保证进程崩溃（`kill -9`）后已返回 Ok 的 append 落盘。不保证断电（需 fsync 目录，属已知局限，见边界声明）。
- `append` 不做 parent 存在性校验、不做树结构校验（保持 O(1) 追加，不读全文件）。

**load / 崩溃恢复流程**：

1. 文件不存在 → 空树（`root: None, nodes: {}`，`next_id = 0`）
2. `BufReader` 逐行 `read_line`，每行 `serde_json::from_str::<SessionEntry>`
3. 某行解析失败（崩溃残留的**半行**）→ **停止读取，忽略该半行及其后**（append-only 下尾半行只可能是最后一条未写完）
4. 收集有效 entries → `reduce(entries) -> SessionTree`
5. `next_id = max(entries.id) + 1`（恢复续写游标）

### reducer（core/session.rs，纯函数，独立单测）

```rust
pub fn reduce(entries: Vec<SessionEntry>) -> Result<SessionTree, SessionError>;
```

规则：

1. 建 `BTreeMap<NodeId, SessionNode>`；`parent_id` 不为 None 的节点，其 parent 必须存在（否则 `ParentNotFound`）
2. `parent_id == None` 的节点为根；**至多一个根**（多个 → `MultipleRoots`）
3. id 唯一（重复 → `DuplicateNode`）
4. 无环（`id` 单调递增 + parent 存在性校验已足以排除环，但实现需显式保证；若有环 → `Cycle`）
5. 按 parent 填充 `children`（按 id 升序，保证顺序稳定）

### SessionRecorder（可选接入，core/session.rs 或独立，实现者定）

不改 003 主循环，复用 001 定稿的 `subscribe()` 事件流做端到端桥接：

```rust
pub struct SessionRecorder {
    storage: Arc<dyn SessionStorage>,
    head: Option<NodeId>,   // 单 lane 游标：上一条已写节点
}

impl SessionRecorder {
    pub fn new(storage: Arc<dyn SessionStorage>) -> Self;
    /// 线性追加：把一条消息挂到当前 head 之后，推进游标。
    /// fork 由调用方显式调用 `fork_at(parent)` 设定新 head（本方法不感知 fork 语义）。
    pub async fn record(&mut self, message: Message) -> Result<NodeId, SessionError>;
    /// 从某历史节点 fork：后续 record 挂到该节点之后。
    pub fn fork_at(&mut self, parent: NodeId);
    /// 接入 001 事件流：对 MessageEnd 逐条串行 record（可选便捷方法）。
    pub async fn attach(&mut self, mut rx: broadcast::Receiver<AgentEvent>);
}
```

- `record` **串行** `await append`（单 lane 游标），保证事件顺序 = 写盘顺序。
- `attach` 仅消费 `MessageEnd { message }` 事件并逐条 `record`（`AgentEnd`/其它事件忽略）；为演示端到端用，是否纳入产品代码由实现者定（至少要有集成测试覆盖）。

### 错误语义（core/session.rs）

```rust
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialize error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("duplicate node id: {0}")]
    DuplicateNode(NodeId),
    #[error("parent node not found: {0}")]
    ParentNotFound(NodeId),
    #[error("multiple roots not allowed")]
    MultipleRoots,
    #[error("cycle detected")]
    Cycle,
}
```

### 边界声明（明确不做，避免过度设计）

- **单 writer / 单进程 / 单 agent**：多 writer 并发 lane（多 agent 写同一 session 日志）属 010 远程协议 / 后续任务；本任务 JSONL 追加无并发互斥需求。
- **跨进程同文件写串行化**（文件锁/flock）不在本任务（006 已声明该边界）。
- **无 checkpoint/快照压缩**：恢复复杂度 O(entries)，全量重放；规模优化属后续。
- **断电级持久性**不保证（不 fsync 目录）；进程崩溃（`kill`）级由 `sync_all` 保证。
- `fork` 的**高层触发时机**（reset/steer 是否 fork、何时 fork）属业务语义，不在本任务定义；本任务只提供「任意 parent 追加 = 分支」的机制 + `fork_at` 游标原语。

## Files

- src/core/session.rs（SessionTree/SessionNode/SessionEntry + SessionStorage trait + JsonlSessionStorage + reduce + SessionRecorder + 单元测试）
- src/core/mod.rs（登记 `pub mod session` + 导出）
- src/lib.rs（re-export `SessionStorage` / `JsonlSessionStorage` / `SessionTree` / `SessionError` 等公开项，遵循既有 facade 惯例）
- tests/session.rs（集成测试，tempdir 驱动）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets -D warnings passes
- [ ] cargo test --all-targets passes
- [ ] cargo fmt --check passes
- [ ] `reduce` 单测：线性链重建；分叉（同一 parent 两条 append）产生两叶子；`path_to` 返回根到叶的正确序列；重复 id → `DuplicateNode`；parent 不存在 → `ParentNotFound`；多根 → `MultipleRoots`
- [ ] `JsonlSessionStorage` 集成测试（tempdir）：
  - 线性 append 若干消息 → `load` 恢复出正确树与 `next_id`
  - **fork**：从历史节点 id append → `load` 后两分支叶子都在
  - **崩溃恢复**：手工写入若干完整行 + 末尾半行 → `load` 忽略半行、正确恢复完整行、`next_id = max+1`
  - 文件不存在 → 空树（`root: None`，`next_id = 0`）
  - 崩溃恢复后继续 `append` → 新 id 不与既有 id 重复（续写正确）
- [ ] `SessionRecorder` 单测/集成测试：`record` 串行推进 head；`fork_at` 后 record 落到 fork 分支；`attach` 从 `broadcast` 事件流逐条落盘（fake 事件驱动）
- [ ] 产品代码无 `unwrap()`；异步测试用 `tokio::test`；tempdir 用 `tempfile`（若 Cargo.toml 缺 dev-dependency 则补 `tempfile` 并说明）
- [ ] 单文件 ≤ 400 行，超则拆子模块并记录

## 修订记录

- v1.0（2026-08-28，Architect）：初稿。落定 architecture 3.8 预留的 `SessionStorage` trait；树结构用 `parent_id` 指针隐式表达（fork = 任意历史节点追加）；append 为 O(1) 追加不校验结构、结构校验集中在 `reduce`；崩溃恢复 = 逐行解析跳半行 + 全量重放 + `next_id` 续写恢复；`sync_all` 保证进程崩溃级持久性（不保证断电）；单 writer 边界声明，多 lane 并发属 010；提供可选 `SessionRecorder` 桥接复用 001 `subscribe()` 事件流做端到端，不改 003 主循环。
