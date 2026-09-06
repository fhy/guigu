# Task 017-b Review - Round 1

## 基本信息
- 审查时间: 2026-09-06
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/017-b-lane-recovery-cwd.md
- 审查提交: c443f72

## 门禁结果
- cargo check: ✓
- cargo clippy --all-targets -- -D warnings: ✓
- cargo test --all-targets: ✓（265 库测试及集成测试通过）
- cargo fmt --check: ✓

## 代码审查
### 问题
1. [Critical] `src/acp/handlers.rs:73-78` — `session/load` 在校验显式 `head` 前先调用
   `load_session_from_factory`，而该调用会把 session 注册到 `AgentServer`。当 `head`
   不存在或是内部节点时，`resume_lane_from_factory` 在第 78 行返回
   `ServerError::Protocol`，但已注册的 session 不会回滚；此后同一 `sessionId` 再次
   `session/load` 会得到 `DuplicateSession`，且注册表中残留一个没有 lane 的 session。
   这使非法请求产生持久的运行时状态污染，也破坏了客户端修正 head 后重试的语义。
   - 建议：将“加载并校验 head”与 session 注册做成事务式流程（例如新增 server 内部
     的 `load_and_resume_session`，先 load tree/校验 head，再原子登记 session 并 spawn
     lane），或在 resume 失败时显式移除本次刚注册的 session 并清理已创建资源；同时
     增加 ACP 非法 head 后使用同一 `sessionId` 重试合法 head 的回归测试。

### 建议
1. `src/acp/handlers.rs:72` — `head` 字段存在但不是 JSON unsigned integer 时被
   `and_then(Value::as_u64)` 静默当作 `None`，请求会退回 max NodeId 叶。建议字段存在
   且类型错误时返回 `AcpError::JsonRpc`，避免调用方拼写/类型错误被悄悄解释为“未指定”。
2. `src/tools/mod.rs:32` — `resolve_tool_path` 的签名可改为 `work_dir: Option<&Path>`，
   避免仅为借用 `Option<PathBuf>` 而传递 `&Option<PathBuf>`，调用意图更清晰；这是可读性
   改进，不影响当前正确性。

## 结论
- [ ] 通过
- [x] 打回

## 下一步
- Developer 需要修复问题 1，并补充对应回归测试。
- 建议一并处理建议 1，明确 ACP 参数校验语义。
