# Task 008 Review - Round 2

## 基本信息
- 审查时间: 2026-08-28
- 审查员: guigu-reviewer
- 审查提交: `6824e17 fix(compactor): Task 008 r1 打回修复`
- 任务规格: `docs/tasks/008-compactor.md`

## 门禁结果
- cargo check: ✓
- cargo clippy --all-targets -- -D warnings: ✓
- cargo test --all-targets: ✓（124 个单元测试；集成测试全部通过）
- cargo fmt --check: ✓

## 代码审查
### 已修复问题
1. `src/core/compactor.rs:100-109` 已将待压缩消息通过
   `format_messages_for_summary` 序列化为单条 `User` 输入，且测试断言了完整请求结构，
   符合规格中的默认拼接格式。
2. `src/core/compactor.rs:148-152` 已将流自然结束或仅 `Done` 的空文本结果判定为
   `CompactionError::EmptySummary`，避免空摘要静默丢失历史上下文；对应测试已补充。
3. `src/core/context.rs:126-130` 已明确摘要/降级结果回写 transcript 的持久化语义，
   并在单元测试及 `tests/compactor.rs` 集成测试中验证后续 turn 不重复压缩且不恢复旧消息。

### 问题
无阻断性代码问题。

### 建议
1. `docs/tasks/008-compactor.md:87-89,187` — 规格伪代码及验收标准仍写着
   `context.messages = req.messages（原样）` / `messages == 待压缩消息`，与同一规格
   `102、160-170` 行规定的“序列化为单条 user 输入”不一致。当前实现采用后者，代码与
   默认拼接格式章节一致；建议由 `guigu-planner` 修订旧文字，避免后续审查或实现继续
   按冲突条款解释。

## 结论
- [x] 通过
- [ ] 打回

## 下一步
- 请 `guigu-planner` 同步修订 Task 008 中关于 provider 消息格式的冲突描述，并补充
  `EmptySummary` 语义；该事项不阻断本次代码通过。
