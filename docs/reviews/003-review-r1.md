# Task 003 Review - Round 1

## 基本信息
- 审查时间: 2026-08-25
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/003-runtime-engine.md

## 门禁结果
- cargo check: ✓
- cargo clippy: ✓ (0 warning)
- cargo test: ✓ (38 tests 全绿，含 Task 003 的 10 个)
- cargo fmt: ✓

## 体量检查
- 文件: mod.rs 356 / turn.rs 333 / tools.rs 215 / step.rs 139 / agent_runtime.rs 132（均 <400） ✓
- 函数: 无超 80 行函数 ✓
- 测试文件: runtime_loop.rs 10 个 `#[tokio::test]`（<30） ✓

## 代码审查

### 问题

1. [Medium] tests/runtime_loop.rs:554-593 — `test_abort` 未断言 `stop_reason == Aborted`
   - 影响: 任务规格明确要求 "abort 后产出 stop_reason: Aborted"，测试只断言 `!snapshot.is_streaming`，未验证 `stop_reason`。属于断言不充分（假绿风险）。
   - 建议: 在 `test_abort` 末尾断言最后一条 assistant 消息的 `stop_reason == Some(StopReason::Aborted)`。

2. [Medium] src/core/runtime/mod.rs:349-356 — `stop_reason_for_error` 是死代码（`#[allow(dead_code)]`）
   - 影响: 该函数未被任何代码调用。`stream_failure_result`（turn.rs:235-259）手动构造 `stop_reason`，未复用此 helper。违反"无 dead code"原则。
   - 建议: 删除 `stop_reason_for_error`，或在 `stream_failure_result` 中复用它。

3. [Low] src/core/runtime/turn.rs:208-231 — `TurnAccumulator::finalize()` 无显式终态分支时默认 `Completed`
   - 影响: 当流结束但未收到 `Done` 事件（provider 异常截断）时，`finalize()` 返回 `stop_reason: Completed`，掩盖了异常。真实场景概率低，但属于逻辑缺陷。
   - 建议: 在 `finalize()` 中增加 `else if self.final_message.is_none()` 分支，返回 `StopReason::Error`。

4. [Low] src/core/runtime/mod.rs:228 — `build_llm_messages` 无条件 `clone()` 全量 transcript
   - 影响: transcript 较大时产生不必要的内存分配与拷贝。仅在 `transform_context` 钩子存在时才需要 clone（钩子消耗所有权），默认预算路径可借用。
   - 建议: 默认路径改为借用 `&ctx.transcript` 做 `truncate`（需要 `truncate` 签名接受 `&[Arc<Message>]` 返回 owned），或使用 `Cow` 避免不必要 clone。

5. [Low] src/core/runtime/turn.rs:41 — `stream_with_retry` 每次重试 `request.clone()`
   - 影响: `ProviderRequest` 含 `Context`（messages + tools），大 transcript 时 clone 成本高。重试场景通常 provider 失败快速返回，实际影响有限。
   - 建议: 可接受（一期），二期可考虑 `Arc<ProviderRequest>` 或 lazy clone。

### 建议

1. src/core/runtime/tools.rs:98-105 — `on_update` 闭包为每次工具调用 clone 事件通道和元数据
   - 一期可接受。二期若工具并发量大，可考虑用 `Arc` 共享共享数据。

2. src/core/tool.rs:48-52 — `ToolError` 未使用 `thiserror`
   - 当前手动实现 `Display` + `Error`，功能正确。项目约定用 `thiserror`，建议保持一致。

## 架构审查

### 并发模型
- **单 writer**: 唯一 runtime task 拥有全部状态（transcript、queue、snapshot），无 `Arc<RwLock>` 竞态 ✓
- **sent/processed 同步**: `wait_for_idle` 以计数对齐为同步点，5s 超时兜底 ✓
- **CancellationToken 贯穿**: provider 流 + 退避等待 + 工具执行均可取消 ✓

### 工具并发编排
- `ReadOnlyParallel`: `FuturesUnordered` 同一 task 内并行，按原序收集 ✓
- `Exclusive`: 打断 ReadOnly 组，独占执行 ✓
- `after_tool_call` 钩子在并行后按原序应用 ✓

### 重试契约
- 仅重试 `provider.stream()` 建立失败，不重试工具 ✓
- 指数退避 `0.5s·2^n`，可取消 ✓

### 事件契约
- `MessageStart` / `MessageUpdate` / `MessageEnd` 包裹流式消息 ✓
- `TurnStart` / `TurnEnd` 包裹 turn ✓
- `ToolExecutionStart` / `ToolExecutionUpdate` / `ToolExecutionEnd` 包裹工具执行 ✓

### 与 Task 001 的衔接
- `agent_runtime.rs` 正确使用 `RuntimeState` 捆绑状态，`process_command` / `run_with_initial` 签名合理 ✓
- `AgentCommand::Abort` 在单命令串行处理中为空操作（run 期间通过 `drain_commands` 取消） ✓
- `AgentCommand::Reset` 清空 transcript + queue，processed 计数正确计入 discarded ✓

## 结论
- [ ] 通过
- [x] 打回（问题 1+2 需修复，3-5 建议修复）

## 下一步
Developer 需修复的问题列表：
1. `tests/runtime_loop.rs` — `test_abort` 补充 `stop_reason == Aborted` 断言
2. `src/core/runtime/mod.rs` — 删除 `stop_reason_for_error` 或在 `stream_failure_result` 中复用
3. `src/core/runtime/turn.rs` — `finalize()` 无 `Done` 时返回 `Error` 而非 `Completed`
4. （建议）`src/core/runtime/mod.rs` — `build_llm_messages` 默认路径避免不必要 clone
5. （建议）`src/core/tool.rs` — `ToolError` 改用 `thiserror` 派生
