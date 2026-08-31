# Task 008 Review - Round 1

## 基本信息
- 审查时间: 2026-08-27
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/008-compactor.md

## 门禁结果
- cargo check: ✓
- cargo clippy --all-targets -- -D warnings: ✓
- cargo test --all-targets: ✓（121 个单元测试及全部集成测试通过）
- cargo fmt --check: ✓

## 代码审查
### 问题
1. [Major] src/core/compactor.rs:91-103 — `LlmCompactor` 从未调用
   `format_messages_for_summary`，因此实际发送给 provider 的仍是多条原始
   `Message`，没有实现规格 008 规定的「按默认格式序列化为 provider user
   输入」。当前测试也只断言原始消息，无法覆盖该行为。
   - 影响: 真实摘要模型收到的上下文不是约定的 `[user] ...`、`[assistant] ...`
     等稳定摘要输入；公开 formatter 变成未接入的死功能。规格第 102、160-170
     行及验收标准第 191 行的语义未完整实现。
   - 建议: 请与 planner 确认规格中第 88 行“原样消息”和序列化要求的冲突；若以
     “默认拼接格式”作为实现契约，应构造一条 `User` 文本消息（文本为
     `format_messages_for_summary(&req.messages)`）再调用 provider，并将单测改为
     断言该单条 user 输入。若坚持原样消息，则应修改任务规格并删除/重新定义
     formatter 验收项，不能同时声称两者均已实现。

### 建议
1. src/core/runtime/mod.rs:283-302 — 摘要/降级结果被持久化回 transcript，虽然
   与当前备注中的设计决策一致，但降级截断会永久丢失旧消息。建议在 API 文档中
   明确这是有意的持久化语义，并增加“同一 run 后续 turn 不重复压缩/不会恢复旧消息”
   的测试，避免调用方误以为只是单次请求投影。
2. src/core/compactor.rs:137 — provider 流自然结束或只返回 Done 而没有任何文本时
   仍返回成功的空摘要。建议视产品语义决定是否将空摘要视为错误并触发降级，至少
   增加对应测试，防止摘要消息静默吞掉历史上下文。

## 结论
- [ ] 通过
- [x] 打回

## 下一步
- @guigu-worker 请修复上述 Major 问题，并补充与最终 formatter 语义一致的
  `LlmCompactor` 请求断言测试；若规格冲突无法自行裁定，请 @guigu-planner 确认。
