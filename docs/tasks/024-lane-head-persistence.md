# Task 024: 持久化 lane head / 活动分支元数据

## Background

015 r2 与 017-b 明确遗留：多 lane fork 场景下，进程重启后「活动 lane 的真实 head」无法表达。当前恢复链路依赖「最大 NodeId 推断活动叶」（015 默认）或调用方显式传 `head`（017-b 方案 B），**lane 自身的 head 指针不落盘**——`LaneWriter.head` 是纯内存游标（012），`SessionEntry` 无 lane 归属字段，`SessionState.lanes` 是进程内注册表（013），进程退出即丢。

017-b 边界声明「不持久化 lane head / 活动分支元数据，属后续任务」即本任务。目标：把「每个 lane 的活动分支头」作为 **append-only 元数据**持久化进 session 日志，恢复时自动重建 `lane_id → head` 映射，使多 lane fork 恢复不再依赖最大 NodeId 推断或外部记忆。

## Goal

- 新增 `LaneHeadRecord`（JSONL 行，append-only）：`lane_id → head: Option<NodeId>`
- 新增可选能力 trait `LaneHeadStore`（写入 + 重放读取），`JsonlSessionStorage` 实现
- `LaneWriter` 向后兼容扩展：可选绑定 `LaneHeadStore`，append/fork 后落盘 head
- 恢复入口（017-b `resume_lane_from_factory` / 013 `load_session`）在 `head: None` 时优先用持久化 head，无记录则回退最大 NodeId 叶（保持 015 行为）
- **向后兼容**：旧 JSONL（仅 Message 行）load 行为不变、`load_lane_heads` 返回空表

## Design Notes

### 契约复用（勿改）

- `NodeId = u64`、`SessionTree`/`SessionNode`/`SessionEntry`、`reduce`、`SessionError`（009 定稿，**形状不动**）
- `SessionStorage` trait（009）：`append`/`load`/`next_id` 签名**不动**
- `SharedSessionStorage`（012/017-a 加固后，无 `inner()`）、`LaneWriter`/`LaneId`（012）
- `Message`（002）已 `Serialize + Deserialize`

### 数据结构（core/session.rs）

```rust
/// lane head 变更记录：某 lane 活动分支头指针（JSONL 一行，append-only）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaneHeadRecord {
    pub lane_id: LaneId,
    pub head: Option<NodeId>,   // None = lane 已建但尚无节点
}

/// JSONL 每行：消息节点 或 lane head 元数据。
/// 用 untagged + 字段互斥区分：Message 无 lane_id/head；LaneHead 无 id/parent_id/message。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SessionRecord {
    Message(SessionEntry),      // 先尝试（绝大多数行）
    LaneHead(LaneHeadRecord),   // 后尝试
}
```

- **字段互斥是 untagged 可靠的前提**：`SessionEntry = {id, parent_id, message}`，`LaneHeadRecord = {lane_id, head}`，二者字段集合不重叠。序列化时 Message 输出 009 裸形状（无 tag、与旧文件一致），LaneHead 输出 `{lane_id, head}`。
- 未来若给 `SessionEntry` 增加 `lane_id` 字段将破坏互斥，需同步改用显式 tag——此约束写入该处 doc 注释。

### LaneHeadStore trait（core/session.rs）

```rust
/// lane head 元数据持久化（可选能力，仅持久化后端实现；内存后端可 no-op/内存表）。
#[async_trait]
pub trait LaneHeadStore: Send + Sync {
    /// append-only 追加一条 lane head 变更（后写覆盖先写，重放取最终值）。
    async fn append_lane_head(&self, lane_id: LaneId, head: Option<NodeId>)
        -> Result<(), SessionError>;
    /// 重放全量 LaneHead 记录，得 lane_id → 最终 head 表（崩溃恢复入口）。
    async fn load_lane_heads(&self) -> Result<HashMap<LaneId, Option<NodeId>>, SessionError>;
}
```

- 复用 009 `SessionError`，不新增变体；序列化失败经 `Serde`、IO 失败经 `Io` 上抛。

### JsonlSessionStorage 实现（core/session.rs 或 session/jsonl.rs）

- **append_lane_head**：`serde_json::to_string(&LaneHeadRecord {..})` → `O_APPEND` 追加一行 + `sync_all`（与 009 append 同一写路径、同一原子性保证）。
- **load_lane_heads**：逐行解析为 `SessionRecord`，仅收集 `LaneHead` 变体，按 `lane_id` 覆盖（后写覆盖先写）得最终表；解析失败（半行）停止，与 009 load 同规则。
- **load() 兼容**：`JsonlSessionStorage::load()` 的逐行解析由 `SessionEntry` 改为 `SessionRecord`，**忽略 `LaneHead` 变体**、只对 `Message` 变体做 `reduce`。旧文件（全 Message 行）结果与 009 完全一致。
- `InMemorySessionStorage`：可选实现 `LaneHeadStore`（内存 `HashMap`），实现者定；不实现则无法绑定 `with_head_store`，行为等价 012。

### LaneWriter 向后兼容扩展（core/session.rs）

```rust
pub struct LaneWriter {
    storage: Arc<dyn SessionStorage>,
    lane_id: LaneId,
    head: Option<NodeId>,
    head_store: Option<Arc<dyn LaneHeadStore>>,   // 新增可选字段
}

impl LaneWriter {
    // 012 原签名不变：head_store = None，行为与 012 完全一致
    pub fn new(storage: Arc<dyn SessionStorage>, lane_id: impl Into<String>, head: Option<NodeId>) -> Self;

    // 新增：绑定 lane head 持久化
    pub fn with_head_store(
        storage: Arc<dyn SessionStorage>,
        lane_id: impl Into<String>,
        head: Option<NodeId>,
        head_store: Arc<dyn LaneHeadStore>,
    ) -> Self;

    /// 新增：显式落盘当前 head（lane 创建/fork 后调用一次，幂等）。
    pub async fn persist_head(&self) -> Result<(), SessionError>;

    // append：追加节点后，若 head_store 存在则自动 append_lane_head(lane_id, new_head)
    // fork_at：保持同步签名（012 原样），只改内存 head，不落盘（由后续 append/persist_head 落盘）
}
```

- `persist_head` 用 `&self`：`head_store` 是 `Arc`，写盘不持跨 await 锁（与 012 `write_lock` 只在 `storage.append` 内串行一致）。
- **落盘时机**：`append` 成功后自动落盘（内聚覆盖 append 推进）；`fork_at` 保持纯内存（不改 012 同步签名），spawn/fork 后由调用方显式 `persist_head` 落初始 head。
- 013 `spawn_lane`/`fork_lane` 建 `LaneWriter` 后：若启用持久化，调 `persist_head().await` 记录初始 head。

### 恢复语义（017-b 衔接，src/server/lane.rs 或 lane_recovery.rs）

- `resume_lane_from_factory`（或等价恢复入口）`head: None` 分支改为：先 `load_lane_heads()` 查 `lane_id` 对应 head——
  - 命中 `Some(h)`：走既有 `head: Some(h)` 路径（transcript = `path_to(h)`、`LaneWriter` head = `h`）
  - 命中 `None`：空 lane，transcript 为空、head = None
  - 未命中（无记录）：回退最大 NodeId 叶（保持 015 行为）
- `head: Some(h)` 显式传入时行为不变（017-b 定稿）。
- `h` 非法（不在树中/内部节点）仍返回 `ServerError::Protocol`（017-b 契约不变）。

### 边界声明（明确不做）

- **不改** 009 `SessionStorage` trait 签名、`SessionEntry`/`Message` 序列化形状、`reduce` 逻辑。
- **不改** 012 `LaneWriter::new`/`fork_at` 同步签名（仅新增可选字段与方法）。
- **lane 关闭/删除**（`head: None` 的主动触发，如 abort/shutdown 时写 `None`）不在一期做：`head: Option<NodeId>` 的 `None` 仅表示「空 lane 初始态」，高层 lane 生命周期收尾属后续。
- **跨进程多写者**（文件锁）不在本任务（009/006/012 已声明）；lane head 持久化仅进程内多 lane 单写者边界。
- 不引入 checkpoint/压缩：恢复复杂度 O(entries) 全量重放（与 009 一致）。

## Files

- src/core/session.rs（`LaneHeadRecord`/`SessionRecord`/`LaneHeadStore` + `LaneWriter` 扩展 + 单测）
- src/core/session/jsonl.rs（`JsonlSessionStorage` 实现 `LaneHeadStore`、`load`/`load_lane_heads` 的 `SessionRecord` 解析，若 009 已拆分至此）
- src/server/lane.rs 或 lane_recovery.rs（恢复入口 `head: None` 优先查持久化 head）
- src/core/mod.rs / src/lib.rs（re-export 新公开项，遵循既有 facade 惯例）
- tests/session.rs 或 tests/session_concurrency.rs（集成测试）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets -D warnings passes
- [ ] cargo test --all-targets passes
- [ ] cargo fmt --check passes
- [ ] `SessionRecord` 单测：`LaneHeadRecord` 与 `SessionEntry` 序列化 roundtrip；字段互斥（Message 行不误判为 LaneHead，反之亦然）
- [ ] `JsonlSessionStorage` + `LaneHeadStore`（tempdir）：`append_lane_head` 后 `load_lane_heads` 重放得正确表；同 lane 多次写 head 后取最终值；崩溃半行丢弃
- [ ] **向后兼容**：手工构造仅 Message 行的旧格式文件 → `load` 重建树不变、`load_lane_heads` 返回空表
- [ ] `LaneWriter`：`with_head_store` 下 `append` 自动落盘 head；`persist_head` 显式落盘；`new`（无 store）不落盘、行为与 012 一致
- [ ] 多 lane fork 端到端（tempdir）：两 lane 各 fork 出分支 → 崩溃恢复后 `load_lane_heads` 得两 lane 各自 head → 分别恢复各自 transcript 与 head
- [ ] 恢复入口：`head: None` 且存在持久化 head → 用持久化 head；无记录 → 回退最大 NodeId（回归 015 行为）
- [ ] 产品代码无 `unwrap()`；异步测试用 `tokio::test`；单文件 ≤ 400 行，超则拆子模块并记录

## 修订记录

- v1.0（2026-09-06，Architect）：初稿。落地 017-b「持久化 lane head 属后续任务」遗留。`LaneHeadRecord`（append-only，`lane_id → head: Option<NodeId>`）+ `LaneHeadStore` 可选 trait（`JsonlSessionStorage` 实现）+ `LaneWriter` 向后兼容扩展（`with_head_store`/`persist_head`，`new`/`fork_at` 签名不动）+ 恢复入口 `head: None` 优先查持久化 head、无记录回退最大 NodeId。零破坏：SessionStorage trait、SessionEntry/Message 序列化、reduce 均不动；JSONL 用 untagged + 字段互斥区分 Message/LaneHead，旧文件向后兼容。
