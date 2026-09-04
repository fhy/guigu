# Task 017-a: 会话存储并发安全加固

## Background

012 已交付 `SharedSessionStorage`（`tokio::sync::Mutex` 串行化 append）+ `LaneWriter`（每 lane 一个 head 游标），是 013/014/015 多 lane 调度的底座。012 r1 审查提出 4 条非阻塞建议，均为会话存储层的并发安全与边界表达问题，本任务集中收尾：

1. `inner()` 暴露裸 `Arc<dyn SessionStorage>`，调用方可用 `LaneWriter::new(inner().clone())` 绕过写锁（footgun）。
2. `LaneWriter` 接受任意 `Arc<dyn SessionStorage>`，类型系统无法保证走的是共享写入口。
3. 并发测试只覆盖星型拓扑（多 lane 各写一条），未覆盖每 lane 多步连续写。
4. 模块级边界说明仍写「多 lane 并发写属 010」，与 012 已交付事实不一致。

本任务**不新增功能**，只做类型边界收紧 + 受控访问 + 测试补齐 + 文档订正。

## Goal

- 移除 `SharedSessionStorage::inner()`，杜绝绕过写锁的入口。
- `LaneWriter` 的 storage 约束为 `Arc<SharedSessionStorage>`，用类型系统强制共享写入口。
- 补每 lane 多步连续写的并发测试。
- 订正模块级边界说明。

## Design Notes

### 契约复用（勿改）

- `SessionStorage` trait（009 定稿）：`append(parent_id, message) -> Result<NodeId, SessionError>`、`load() -> Result<SessionTree, SessionError>`、`next_id() -> NodeId`。
- `SharedSessionStorage` 已实现 `SessionStorage`（append 持锁串行化；load/next_id 透传）。这一事实是本任务收紧类型的前提：调用方需要 `Arc<dyn SessionStorage>` 时，`Arc<SharedSessionStorage>` 本身就是合法入参，**不需要** `inner()`。
- `LaneWriter` 其余语义（append 推进 head、fork_at 换 head）不变。

### 1. 移除 `inner()`

- 删除 `SharedSessionStorage::inner()` 公开方法（字段 `inner` 保留为私有）。
- **迁移所有调用点**：全仓 grep `\.inner()`，凡取裸 storage 者改为直接使用 `Arc<SharedSessionStorage>`（其自身实现 `SessionStorage`，`load()`/`next_id()` 透传、`append()` 持锁）。禁止任何调用方再拿到裸 `Arc<dyn SessionStorage>`。
- 若确有「只读」需求（如恢复入口 `load()`），调用方直接 `shared.load().await`，不经过任何裸引用。

### 2. `LaneWriter` 类型约束

- `LaneWriter` 的 `storage` 字段由 `Arc<dyn SessionStorage>` 改为 `Arc<SharedSessionStorage>`。
- `LaneWriter::new(storage: Arc<SharedSessionStorage>, lane_id: impl Into<String>, head: Option<NodeId>)`。
- 迁移所有 `LaneWriter::new` 调用点（013/014/015 及测试）：入参统一为 `Arc<SharedSessionStorage>`。
- **桥接机制（方案C，v1.1）**：`srclane` 用 `SessionState.storage` 构造 `LaneWriter`，故 `SessionState.storage` 改为 `Arc<SharedSessionStorage>`（server 内部类型）。但 server 公共入口 `StorageFactory`/`with_storage_factory`/`create_session`/`load_session` **签名保持 `Arc<dyn SessionStorage>` 不变**——在 `create_session`/`load_session` 边界一次性 `Arc::new(SharedSessionStorage::new(storage))` 包裹后存入 `SessionState`。否决方案A（改公共签名为 `Arc<SharedSessionStorage>`，无必要的 breaking change）；否决方案B（运行时 downcast，引入失败路径）。

### 3. 每 lane 多步写并发测试

在 `tests/session_concurrency.rs` 新增（独立文件，复用 012 既有 tempdir 组合，`SharedSessionStorage` 包 `JsonlSessionStorage`；共享 helper 提取到测试辅助模块，避免把 `tests/session.rs` 推超 400 行）：

- 两个 `LaneWriter` 共享同一 `Arc<SharedSessionStorage>`，从同一初始 head 各自**连续** append 3 条消息（`join_all` 并发跑两个 lane，lane 内顺序 await）。
- 断言：`load()` 后共 7 节点（1 个初始 head + 每 lane 3 条）；每个 lane 的 3 条消息形成各自的单链（第 i+1 条 parent = 第 i 条）；两条链的第一条互为兄弟（同 parent）；id 全局唯一；无交错半行（JSONL 完整）。

### 4. 边界说明订正

- `src/core/session.rs` 模块级 doc comment 中「多 lane 并发写属 010」改为「多 lane 并发写已由 012 交付（仅进程内，`SharedSessionStorage` 串行化 append + `LaneWriter` 每 lane 游标）」。
- 同步核对 013/014/015 相关 doc comment 是否仍有同类过期描述，一并订正（仅注释，不改逻辑）。

## Files

- src/core/session.rs（移除 `inner()`、`LaneWriter` 类型约束、边界说明订正、单测）
- src/core/mod.rs / src/lib.rs（若 re-export 含 `inner` 相关符号则清理；`LaneWriter` re-export 不变）
- src/server/（`SessionState.storage` 改 `Arc<SharedSessionStorage>`，`create_session`/`load_session` 边界包裹 `SharedSessionStorage::new`；公共入口签名保持 `Arc<dyn SessionStorage>` 不变）；src/acp/、src/bin/（仅迁移 `inner()` / `LaneWriter::new` 调用点）
- tests/session_concurrency.rs（新增每 lane 多步写并发测试，独立文件）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets -D warnings passes
- [ ] cargo test --all-targets passes
- [ ] cargo fmt --check passes
- [ ] `SharedSessionStorage` 不再有公开 `inner()`；全仓 grep `.inner()` 无残留（会话存储相关）
- [ ] `LaneWriter::new` 仅接受 `Arc<SharedSessionStorage>`；所有调用点已迁移，无裸 `Arc<dyn SessionStorage>` 注入 lane
- [ ] 每 lane 多步写并发测试通过：每 lane 3 条成链、两链首节点同 parent、id 唯一、JSONL 无交错半行
- [ ] 模块级边界说明已订正为「多 lane 仅进程内（012 交付）」，无过期描述
- [ ] 既有 012/013/014/015 相关测试全绿（无破坏性回归）
- [ ] 产品代码无 `unwrap()`；异步测试用 `tokio::test`；单文件 ≤ 400 行

## 修订记录

- v1.0（2026-09-05，Architect）：初稿。打包 012 r1 四条非阻塞建议：移除 `inner()` 杜绝绕过写锁、`LaneWriter` storage 约束为 `Arc<SharedSessionStorage>` 强制共享写入口、补每 lane 多步写并发测试、订正模块级边界说明。不新增功能，纯加固。
- v1.1（2026-09-05，Architect，依据 Developer 架构审查 + Reviewer r1）：① 明确桥接机制为方案C——`SessionState.storage` 改 `Arc<SharedSessionStorage>`（内部），server 公共入口签名保持 `Arc<dyn SessionStorage>` 不变，`create_session`/`load_session` 边界 `Arc::new(SharedSessionStorage::new(storage))` 包裹；否决方案A（改公共签名，无必要 breaking change）与方案B（运行时 downcast）；② 订正 §3 节点数 6→7（1 根 + 每 lane 3 条）；③ 测试拆至独立文件 `tests/session_concurrency.rs`（避免 `tests/session.rs` 超 400 行）。
