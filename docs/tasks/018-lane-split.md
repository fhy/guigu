# Task 018: server/lane.rs 超限拆分（恢复事务逻辑 + 共享 helper 独立成模块）

## Background

017-b 交付多 lane 恢复语义 + 工作目录隔离后，`src/server/lane.rs` 达 420 行，超过 conventions.md §Size Limits 的 400 行单文件上限。017-b r2 审查（docs/reviews/017-b-review-r2.md）已将其列为技术债并建议「拆恢复事务逻辑 / 共享 helper 至独立模块」。本任务纯重组：只拆不改进，不改产品行为与公开契约。

## Goal

- `src/server/lane.rs` 及拆分后的每个新文件均 ≤ 400 行。
- 按职责拆分：lane 调度核心留在主模块；恢复事务逻辑（session/load 事务化 + resume/fork 装载会话树 + spawn 失败回滚）拆至独立模块；可复用 helper 拆至共享模块。
- 零行为变化：不改公开 API、不改恢复/lane 调度语义、不增删测试。

## Design Notes

### 契约复用（勿改）

- `LaneWriter`、`SharedSessionStorage`（012 定稿）语义不变。
- 017-b 定稿恢复事务语义不变：`session/load` 事务化（先校验 `head` 再注册）、spawn 失败回滚空 session、`path_to(h)` 仅限叶节点契约、工具 `work_dir` 显式传参。
- `AgentServer` 公共入口签名不变（017-a 定稿：`Arc<dyn SessionStorage>` 边界包裹，`create_session`/`load_session` 在边界 `Arc::new(SharedSessionStorage::new(...))` 包裹）。

### 拆分策略

- 保持 `src/server/` 目录不变，新增子模块文件（命名沿用既有 snake_case 风格，如 `lane_recovery.rs` / `lane_helper.rs`，或 `lane/` 子目录 + `mod.rs`，二选一由 Developer 依据实际代码结构定夺）。
- 跨 server 内部模块用 `pub(crate)` / `pub(super)` 可见性，不扩大到 crate 公共面。
- 拆分仅重组：函数/类型/常量移动 + `use` 路径调整，**不改逻辑、不重写控制流、不合并/拆分函数实现**。
- 若 `lane` 现为外部可见模块，主文件用 `pub use` 重导出保持外部路径不变（外部 `use guigu::server::lane::X` 不得破坏）。

### 边界

- 不触碰 `src/tools/`、`src/plugin/`、`src/acp/`、`src/core/` 等其他模块。
- 不引入新依赖。
- 测试仅移动不删改；测试总数不减、全绿。

## Files

- src/server/lane.rs（缩减至 ≤ 400 行，保留对外公共入口 + `pub use` 重导出）
- src/server/lane_recovery.rs（恢复事务逻辑，新，建议命名）
- src/server/lane_helper.rs（共享 helper，新，建议命名，按需）
- src/server/mod.rs（子模块声明）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets -D warnings passes
- [ ] cargo test --all-targets passes（测试总数不减）
- [ ] cargo fmt --check passes
- [ ] `src/server/lane.rs` 及所有新增 server 模块文件均 ≤ 400 行
- [ ] 公开 API 无破坏：`AgentServer` 公共入口、`LaneWriter`/`SharedSessionStorage`、`lane` 外部可见路径签名均不变
- [ ] 恢复事务语义回归测试全绿（017-b 既有测试覆盖 `session/load` 事务化 + 回滚）
- [ ] 产品代码无 `unwrap()`；单文件 ≤ 400 行

## 修订记录

- v1.0（2026-09-06，Architect）：初稿。承接 017-b r2 技术债（lane.rs 420 行超限），纯重组拆分，零行为变化。建议文件名仅为占位，Developer 依据实际代码结构选定最终模块边界与命名。
