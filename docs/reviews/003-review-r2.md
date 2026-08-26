# Task 003 Review - Round 2

## 基本信息
- 审查时间: 2026-08-26
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/003-runtime-engine.md
- 审查对象: fb36c12 fix(runtime): Task 003 r1 修复审查问题

## 门禁结果
- cargo check: ✓
- cargo clippy: ✓ (0 warning)
- cargo test: ✓ (39 tests 全绿，含 Task 003 的 11 个)
- cargo fmt: ✓

## 体量检查
- 文件: mod.rs 357 / turn.rs 332 / tools.rs 215 / step.rs 139 / agent_runtime.rs 132 / tool.rs 108 / context.rs 151（均 <400） ✓
- 函数: 无超 80 行函数 ✓
- 测试文件: runtime_loop.rs 11 个 `#[tokio::test]`（<30） ✓

## 代码审查 — 逐项验证

### 1. [R1-1] test_abort 补充 stop_reason 断言 ✅
- `tests/runtime_loop.rs:594-605`
- 从 `snapshot.messages.last()` 取最终 assistant 消息，模式匹配 `Message::Assistant(a)`，断言 `a.stop_reason == Some(StopReason::Aborted)`
- 断言充分，无假绿风险

### 2. [R1-2] stop_reason_for_error 死代码消除 ✅
- `src/core/runtime/mod.rs:351-357` — `#[allow(dead_code)]` 已删除，`pub(crate)` 签名保留
- `src/core/runtime/turn.rs:23` — 导入 `stop_reason_for_error`
- `src/core/runtime/turn.rs:243` — `stream_failure_result` 复用该函数：`Some(stop_reason_for_error(aborted))`
- 死代码消除 + 复用，干净利落

### 3. [R1-3] finalize() 无 Done 时返回 Error ✅
- `src/core/runtime/turn.rs:208-233` — `finalize()` 三分支结构：
  1. `final_message.is_some()` → 直接返回 provider 给的消息
  2. `aborted` → `StopReason::Aborted`
  3. `error_message.is_some()` → `StopReason::Error` + 传播错误
  4. else（流异常截断）→ `StopReason::Error` + "stream ended without Done event"
- 四条路径均正确覆盖，不再掩盖异常为 Completed

### 4. [R1-4] build_llm_messages 默认路径借用优化 ✅
- `src/core/runtime/mod.rs:230-238` — `if let Some(hook)` 才 clone transcript（钩子消耗所有权），默认路径 `ctx.transcript.as_slice()` 借用
- `src/core/context.rs:77` — `truncate(&self, messages: &[Arc<Message>]) -> Vec<Arc<Message>>` 签名改为借用切片，内部 `messages.to_vec()` 仅 clone 保留的 Arc 指针
- `context.rs` 中两处测试同步更新，调用 `budget.truncate(&msgs)` 借用
- 类型衔接：`truncate` 返回 `Vec<Arc<Message>>`，直接传入 `convert_to_llm(Vec<Arc<Message>>)` ✓

### 5. [R1-5] ToolError 改用 thiserror ✅
- `src/core/tool.rs:50-54` — `#[derive(Error, Debug, Clone, Serialize, Deserialize)]` + `#[error("ToolError: {message}")]`
- 手动 `Display` + `Error` impl 已删除，对齐 `provider.rs` 既有写法
- `Clone + Serialize + Deserialize` 保留，不影响既有使用

### 6. [R1 补充] 新增 no-Done → Error 行为测试 ✅
- `tests/runtime_loop.rs:609-645` — `test_stream_ends_without_done`
- 构造仅 `TextDelta` 无 `Done` 的 turn → 流直接结束
- 断言 `stop_reason == Some(StopReason::Error)` + `error_message.is_some()`
- 覆盖 Fix 3 新增的 else 分支，测试有效

## 架构审查

### R1 遗留问题
- **问题 5**（`stream_with_retry` 每次重试 `request.clone()`）：Developer 按规格标注"一期可接受"，未改动。合理。

## 结论
- [x] 通过
- [ ] 打回

## 下一步
R1 提出的 5 个问题已全部修复，无遗留。Task 003 可关闭。
