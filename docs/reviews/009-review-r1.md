# Task 009 Review - Round 1

## 基本信息

- 审查时间: 2026-08-31
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/009-session-tree.md
- 提交: 8f15831 feat(session): Task 009 Session 树 + JSONL 崩溃恢复

## 门禁结果

- cargo check: ✓
- cargo clippy -- -D warnings: ✓
- cargo test: ✓（137 个单元测试 + 集成测试全部通过）
- cargo fmt --check: ✓

## 代码审查

### 问题

1. **[Warning] `src/core/session.rs:175,224` — 节点 id 达到 `u64::MAX` 时 `max_id + 1` 溢出**
   - 影响: `open()` 或 `load()` 处理合法 JSONL 中的最大 `u64` id 时，在 debug 构建会 panic，在 release 构建会回绕为 0，导致后续 append 产生重复 id，破坏崩溃恢复的核心不重复保证。该值也可由外部可修改的 session 文件注入。
   - 建议: 使用 `checked_add(1)`；增加明确的 `SessionError::IdExhausted`（或等价错误）并在 `open`、`load` 更新游标前返回错误。`append` 也应在游标耗尽时返回该错误，而不是让 `fetch_add` 回绕。补充 `id == u64::MAX` 的回归测试。

2. **[Warning] `src/core/session.rs:66-76` — `path_to` 没有执行“叶节点”契约校验**
   - 影响: 文档和 Task 009 规格将该 API 定义为“从根到某叶”，并说明“叶不存在返回 `None`”；当前传入内部节点（例如有 child 的根）也会返回成功的部分路径，调用方无法区分完整 transcript 和中间路径，容易错误恢复非活跃分支。
   - 建议: 在遍历前检查 `self.nodes.get(&leaf)?.children.is_empty()`，非叶节点返回 `None`；或者若产品确实需要支持任意节点，应同步修改规格/API 文档并增加对应测试，避免语义含混。

## 建议

1. `src/core/session.rs:187-202` — `fetch_add` 在写盘失败后已经消耗 id。当前不会产生重复 id，但会产生空洞；建议在文档中明确这是允许的 monotonic cursor 语义，或在后续任务中通过单 writer 事务化策略处理。
2. `src/core/session.rs:354-367` — `attach` 记录失败仅告警并继续消费事件。由于 API 无返回值，调用方无法知道持久化失败；建议后续提供返回错误/取消订阅的策略，至少增加失败注入测试并明确语义。

## 结论

- [ ] 通过
- [x] 打回

## 下一步

@guigu-worker 请修复问题 1；问题 2 请按规格落实或与 planner 确认后修改契约。补充回归测试并重新运行四道门禁后申请复审。
