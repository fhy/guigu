# Task 015 Review - Round 2

## 基本信息
- 审查时间: 2026-09-04
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/015-cli.md
- 修复提交: `adab5bb`

## 门禁结果
- cargo check: ✓
- cargo clippy --all-targets -- -D warnings: ✓
- cargo test --all-targets: ✓（245 个库测试，全部集成测试通过；CLI 7 个通过）
- cargo fmt --check: ✓

## 代码审查
### 已验证修复
1. `src/server/lane.rs:135-175` — `resume_lane_from_factory` 重新加载 session 树，取最新叶节点，并用根到叶的消息路径初始化 runtime transcript；同时将该叶设置为 `LaneWriter` head。恢复后的 agent 能看到历史，新消息也会接在历史链末尾。
2. `src/core/agent.rs:222-271`、`src/core/agent_runtime.rs:22-79` — 初始 transcript 同时注入 snapshot 和 runtime，避免仅恢复展示状态而未恢复实际模型上下文。
3. `src/bin/guigu/assemble.rs:76-106`、`src/acp/handlers.rs:58-76` — CLI `--session` 与 ACP `session/load` 均使用恢复入口，而新建 session 仍使用空 transcript/空 head。
4. `src/server/tests.rs:211-322`、`tests/cli.rs:139-224` — 新增单元及跨进程测试，验证历史 snapshot、单根 parent 链、续写节点 parent 和完整叶路径。

### 问题
无阻塞问题。

### 建议
1. `src/server/lane.rs:160` — 当前以最大 `NodeId` 的叶作为“活动叶”。该策略在本任务单 lane、单进程写入边界下成立，但 session 树本身允许 fork，多 lane 时并不能表达真正的活动 lane。后续若需要恢复指定分支，建议持久化 lane head/活动分支元数据，或让恢复 API 显式接收目标 head，而不是依赖最大 id 推断。
2. `src/bin/guigu/assemble.rs:49-52` — `set_current_dir` 仍修改进程级全局 cwd。该问题已在 r1 标为非阻塞，当前不影响本次续聊修复；后续 ACP 多 session 场景应将工作目录显式传给工具配置，避免 session 间相互影响。

## 结论
- [x] 通过
- [ ] 打回

## 下一步
- Task 015 Critical 修复已验证，可标记为通过。
- 上述两项属于后续架构改进，不阻塞本任务。
