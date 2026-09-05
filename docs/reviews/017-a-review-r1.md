# Review Task 017-a：会话存储并发安全加固（Round 1）

## 结论：REJECT

## 门禁

- `cargo clippy --all-targets -- -D warnings`：✓（使用 `/home/fhy/.cargo/bin` 环境复核）
- `cargo test --all-targets`：✓（254 个单元测试及全部集成测试通过）
- `cargo fmt --check`：✓
- `git diff --check`：✓

## 问题

### 1. [P1] 不必要地破坏公开 storage API

- 位置：`src/server/mod.rs:44,144-155,176-180`；`src/lib.rs:36`
- `StorageFactory`、`with_storage_factory`、`create_session`、`load_session` 均从
  `Arc<dyn SessionStorage>` 改为 `Arc<SharedSessionStorage>`。这些类型通过
  `pub mod server` / 顶层 API 对嵌入方可见，属于 breaking change；提交说明中
  “本仓无外部嵌入方”无法由仓库测试证明，也与项目“Embeddable”目标不一致。
- 该改动也超出规格 Files 节“`src/server/` 仅迁移调用点”的范围。`LaneWriter`
  收紧为 `Arc<SharedSessionStorage>` 并不要求所有 server 公共入口同步收紧：入口仍可
  接收 `Arc<dyn SessionStorage>`，在 `create_session` / `load_session` 内统一包成
  `Arc<SharedSessionStorage>` 后存入 `SessionState`；工厂也可保留原返回类型并在
  server 边界包装。
- 建议：保留现有公开 API 的 `Arc<dyn SessionStorage>` 签名，在 server 注册边界
  一次性包装为 `Arc<SharedSessionStorage>`；或者先由 Planner/PM 明确批准此次 API
  破坏，并在公开 API 文档和版本策略中记录迁移方案。不要以“当前仓库没有调用方”
  作为兼容性结论。

### 2. [P1] 新增测试使文件违反规格的单文件体量门禁

- 位置：`tests/session.rs:365-439`
- 文件由原来的 363 行增至 439 行；017-a 验收标准第 71 条明确要求单文件 ≤ 400
  行，约定文档第 287 条还要求对 400 行附近文件的大改先拆分。
- 建议：将新增的多 lane 并发场景拆到独立的集成测试文件（例如
  `tests/session_concurrency.rs`），共享测试 helper 时提取到测试辅助模块，确保
  每个文件不超过 400 行，再重新运行四道门禁。

## 已确认事项

- `SharedSessionStorage::inner()` 已移除，仓库未发现会话存储相关 `.inner()` 残留。
- `LaneWriter::storage` 和 `LaneWriter::new` 已收紧为
  `Arc<SharedSessionStorage>`，server 的两个构造点传入类型正确。
- 新增测试确实验证了两条三节点链、共同 parent、唯一 id 及 JSONL 逐行可解析；
  `root + 3 + 3` 断言为 7 节点是正确的。
- `src/core/session.rs` 的模块边界说明已订正；当前门禁和测试均通过。
