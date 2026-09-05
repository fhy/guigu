# Task 017-b: 多 lane 恢复语义 + 工作目录隔离

## Background

015 交付 CLI 独立运行（REPL + `--acp`），实现 `resume_lane_from_factory` 恢复历史 session。015 r2 审查提出两条非阻塞建议，均属多 session / 多 lane 场景的语义缺口，本任务收尾：

1. **lane 恢复依赖「最大 NodeId 推断活动叶」**：单 lane、单进程下成立，但 session 树本身允许 fork，多 lane 时无法表达真正的活动 lane。
2. **`set_current_dir` 改进程级全局 cwd**：多 session 场景下 session 间会相互影响。

## Goal

- 恢复 API 支持**显式指定目标 head**，消除「最大 NodeId 推断」的唯一依赖。
- 工作目录从「进程级全局 cwd」改为「工具配置的显式参数」，session 间隔离。

## Design Notes

### 契约复用（勿改）

- `NodeId = u64`、`SessionTree::path_to(leaf) -> Option<Vec<&Message>>`（009 定稿）。
- `SharedSessionStorage`（012 定稿，017-a 加固后：无 `inner()`）、`LaneWriter`（head 游标）。
- `BashArgs { command, cwd: Option<String>, timeout_ms }`（006 定稿，bash 已支持 `cwd`）。

### 1. 恢复 API 显式收 head（方案 B：显式参数，不持久化元数据）

- `resume_lane_from_factory`（或等价恢复入口）新增参数 `head: Option<NodeId>`：
  - `Some(h)`：以 `path_to(h)` 的根→叶消息序列初始化 runtime transcript + snapshot，`LaneWriter` head 设为 `h`。
  - `Some(h)` 且 `path_to(h)` 返回 `None`（`h` 不在树中，或 `h` 为内部节点）：**显式返回 `ServerError::Protocol`**，不静默回退到最大 `NodeId` 叶，避免掩盖调用方传入非法/过期 head 的 bug。`h` 仅限叶节点——009 契约为 `path_to(leaf)`，内部节点返回 `None`（见 `core::session` 既有测试 `reduce_path_to_internal_node_returns_none`），不扩展 `path_to` 语义；从内部节点开新分支属 `fork_lane`（013），非本恢复入口职责。
  - `None`：保持 015 现状——取最大 `NodeId` 的叶作为活动叶（兼容单 lane 续聊）。
- **调用方更新**：
  - CLI `--session` 续聊：传 `None`（默认回退行为，用户无分支意图）。
  - ACP `session/load`（014）：新增可选 `head` 字段透传；未提供 → `None`。
- **边界声明（明确不做）**：**不持久化 lane head / 活动分支元数据**。lane 拓扑是进程内运行时状态（与 009/012「多 lane 仅进程内」一致）；跨进程/持久化 lane 拓扑需改存储格式，属后续任务。本任务只消除「唯一依赖最大 id」的推断假设，让调用方能显式指定恢复分支。

### 2. 工作目录显式化

- **移除进程级 `set_current_dir`**：删除 `src/bin/guigu/assemble.rs` 中的 `set_current_dir` 调用；全仓 grep 其它 `set_current_dir` 调用点（含 ACP 装配路径），一并移除。
- **文件工具注入工作目录**：`ReadTool` / `WriteTool` / `EditTool` 构造函数新增 `work_dir: Option<PathBuf>`（`None` = 相对路径按进程 cwd 解析，保持旧行为；`Some(d)` = 相对路径 join `d`，绝对路径不变）。路径解析在 `execute` 内完成，工具不再隐式依赖进程 cwd。**解析只做一次**，解析结果（归一化绝对路径）同时用于 `FileMutationQueue` 锁 key 与 IO，保证锁 key 与实际写文件路径一致。
- **bash 工具**：`BashArgs.cwd` 已存在（006），为 per-call 参数，装配期拿不到，无法「装配时填入」。改为 `BashTool::new` 构造注入 `default_cwd: Option<PathBuf>`，`execute` 内 `args.cwd` 为空时回退 `default_cwd`。
- **装配**：CLI 装配处将 session 的工作目录传入文件工具构造与 bash 默认 cwd，替代原 `set_current_dir`。

### 边界声明（明确不做）

- 不引入「每 session 全局 cwd 状态机」；工作目录只作为工具构造/调用参数显式传递，保持纯函数式、无进程级副作用。
- 不持久化工作目录元数据（与 lane head 同理，属上层装配/配置策略）。

## Files

- src/server/lane.rs（`resume_lane_from_factory` 增 `head: Option<NodeId>` 参数 + 分支逻辑）
- src/bin/guigu/assemble.rs（移除 `set_current_dir`；传 work_dir 给工具构造与 bash 默认 cwd）
- src/tools/read.rs / write.rs / edit.rs（构造增 `work_dir: Option<PathBuf>`；相对路径 join）
- src/tools/bash.rs（`BashTool::new` 增 `default_cwd: Option<PathBuf>` 构造参数）
- src/acp/handlers.rs（`session/load` 透传可选 `head`；核对并移除 `set_current_dir`）
- 相关测试（server/lane 恢复分支、文件工具 cwd 解析、CLI 续聊回归）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets -D warnings passes
- [ ] cargo test --all-targets passes
- [ ] cargo fmt --check passes
- [ ] `resume_lane_from_factory` 支持 `head: Some(h)`：transcript = `path_to(h)`、`LaneWriter` head = `h`；`head: None` 回退最大 NodeId 叶（行为与 015 一致）
- [ ] `head: Some(h)` 且 `h` 不在树中或为内部节点：显式返回 `ServerError::Protocol`（非静默回退）
- [ ] 多 lane fork 场景测试：fork 出两个叶后，分别以两个 `head` 恢复，得到各自正确 transcript 与 head
- [ ] 全仓无 `set_current_dir` 残留；文件工具经 `work_dir` 显式解析相对路径；bash 默认 cwd 由装配层填充
- [ ] 既有 CLI `--session` 续聊回归测试全绿（`None` 路径行为不变）
- [ ] 产品代码无 `unwrap()`；异步测试用 `tokio::test`；单文件 ≤ 400 行

## 修订记录

- v1.0（2026-09-05，Architect）：初稿。打包 015 r2 两条非阻塞建议：恢复 API 显式收 `head: Option<NodeId>`（消除唯一依赖最大 NodeId 推断，`None` 保持兼容，不持久化 lane head 元数据）；工作目录由进程级 `set_current_dir` 改为工具构造/调用显式参数（文件工具注入 `work_dir`，bash 沿用 `cwd`），session 间隔离。
- v1.1（2026-09-05，Architect，依据 Developer 预审反馈）：补三处边界——① `path_to(h)` 失败（`h` 不在树中，或为内部节点）显式返回 `ServerError::Protocol`，不静默回退；`h` 仅限叶节点，不扩展 `path_to(leaf)` 009 契约（内部节点开新分支属 `fork_lane`）；② bash 默认 cwd 由「装配时填入」改为 `BashTool::new` 构造注入 `default_cwd`（`BashArgs.cwd` 为 per-call 参数）；③ 文件工具路径解析只做一次，解析结果同用于 FileMutationQueue 锁 key 与 IO。
