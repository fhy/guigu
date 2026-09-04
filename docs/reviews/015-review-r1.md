# Task 015 Review - Round 1

## 基本信息
- 审查时间: 2026-09-04
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/015-cli.md

## 门禁结果
- cargo check: ✓
- cargo clippy --all-targets -- -D warnings: ✓
- cargo test --all-targets: ✓（243 个库测试，集成测试全部通过；CLI 6 个通过）
- cargo fmt --check: ✓

## 代码审查
### 问题
1. [Critical] src/bin/guigu/assemble.rs:71-95 — `--session` 的“续聊”只调用了 `load_session`，随后以空 transcript 的新 `AgentHandle` 创建 lane；`AgentServer::load_session` 也只执行 storage.load，并不会把恢复的消息注入 runtime。更严重的是 `spawn_lane` 使用 `LaneWriter::new(..., None)`，因此恢复后第一条新消息会成为新的根节点，而不是接在既有 session 末尾。这样第二次启动虽然 JSONL 行数增长，但 agent 看不到历史上下文，后续 `load/reduce` 还可能因多个根节点失败，未满足规格中“`--session` 续聊（load_session）”的语义。
   - 影响: CLI 的核心 session 恢复功能实际不可用；现有测试只断言文件行数增长，没有验证历史消息被 runtime 使用或 parent 链保持有效。
   - 建议: 在恢复路径中从 `SessionStorage::load` 得到活动叶路径，使用恢复的 transcript 初始化 `AgentHandle`/`AgentRuntime`，并将活动叶 `NodeId` 作为 `LaneWriter` 的初始 head；或者为 `AgentServer` 增加明确的 `load_session` + `spawn_lane` 恢复 API。补充跨进程 CLI 测试：首进程写入独特文本，第二进程以同一 `--session` prompt，并断言 provider 请求/快照包含首轮消息，同时校验 JSONL 仍为单一有效链。

### 建议
1. src/bin/guigu/assemble.rs:49-52 — `std::env::set_current_dir` 修改整个进程的全局 cwd。单进程 ACP 会话共享该状态，且 `session/new` 的 ACP `cwd` 参数没有被处理。建议将 cwd 作为工具/runtime 配置显式传递，至少拒绝或明确记录与 CLI `--cwd` 冲突的 session cwd，避免多 session 工作目录互相影响。
2. tests/cli.rs:84-134 — 当前“续聊”测试只验证 JSONL 行数增加，属于持久化写入冒烟，不足以证明恢复语义。应增加历史消息可见性、parent 链和多次 load 的断言。

## 结论
- [ ] 通过
- [x] 打回

## 下一步
- @guigu-worker 请修复 Critical 问题，并补充真正验证 session 恢复上下文的测试后重新提交审查。
