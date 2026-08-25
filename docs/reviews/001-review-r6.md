# Task 001 Review (r6)

- 日期: 2026-08-22
- 审查对象: 工作区未提交代码（src/core/agent.rs, tests/agent_lifecycle.rs）
- 结论: **REJECT**

## 结论摘要

积极进展：r4/r5 的 18 个编译错误已全部修复（`#[async_trait]` 已标注、`std::thread::spawn`
已移除、InMemoryAgent 已删除、文件缩至 328 行 ≤ 400 上限）。但本轮引入了新的阻塞级缺陷：
**`cargo test` 死锁**（2 个测试永久挂起），且 `clippy --all-targets` 失败，
核心命令（Reset/Steer/FollowUp/Continue）仍未实现。

## DoD 门禁

| Gate | 结果 |
|------|------|
| cargo check | ✓ |
| cargo clippy -D warnings | ✓（默认 target） |
| cargo clippy --all-targets -D warnings | ✗ 2 errors |
| cargo test | ✗ 死锁：2 个测试挂起 >60s，套件永不结束（timeout 强杀） |
| cargo fmt --check | ✓ |

注：`cargo test` 实测结果 —— `test_prompt_updates_snapshot`、`test_abort_stops_run`、
`test_concurrent_prompt_handling` 通过；`test_wait_for_idle`、`test_reset_clears_transcript`
运行超过 60 秒无响应（死锁）。DoD 要求 `cargo clippy --all-targets` 也须为零告警
（集成测试在 all-targets 范围内）。

## 阻塞问题

1. **`wait_for_idle` 死锁（根因缺陷）** — src/core/agent.rs:184 / 311 用
   `self.idle.notified().await`，而 runtime 在 agent.rs:273 / 277 用
   `idle_clone.notify_waiters()` 发通知。`Notify::notify_waiters()` **不存储许可**：
   通知发出时尚未 `.await` 的等待者将永远收不到。
   - `test_wait_for_idle`：spawn 后直接等待，此前从无任何 notify → 永久挂起。
   - `test_reset_clears_transcript`：prompt 仅入队即返回，sleep(100ms) 期间 runtime
     已处理完毕并发过通知，随后才 `wait_for_idle().await` → 错过通知，永久挂起。
   → 修复建议：放弃裸 Notify 方案，改用 `watch<bool>`（idle 标志，AgentEnd 后置 true）+
   循环 `changed().await` 直到标志为真；或保留 Notify 但先检查状态快照再注册等待，
   并以 `notify_one` 补发。无论哪种方案，须有超时兜底避免测试永久挂起。

2. **`cargo clippy --all-targets -- -D warnings` 失败 ×2**
   - tests/message.rs:7 — `use serde_json;` 冗余导入（clippy::single_component_path_imports）
     → 删除该行。
   - tests/echo_agent.rs:9 — `assert!(true)`（clippy::assertions_on_constants）
     → 该文件是 fake green（r5 第 6 条原样存在）。删除此文件或改为真实 EchoTool 测试。

3. **`test_abort_stops_run` 为 flaky 测试** — tests/agent_lifecycle.rs:35-50。
   abort() 仅 try_send 入队，随后立即 wait_for_idle；若 runtime task 抢先处理完 Abort
   （notify 已发）测试才开始 await → 与第 1 条同样的死锁路径。本轮通过纯属调度时序运气，
   不能视为稳定绿灯。修好 wait_for_idle 后此测试仍需重写语义
   （规格：abort 后 run 结束且状态一致）。

## 规格未实现（阻塞）

4. **Reset / Continue / Steer / FollowUp 四个命令全部未处理** — src/core/agent.rs:283-285
   落入 `_ => {}`。规格 Design Notes 明确 AgentCommand 含这四个变体且行为契约要求
   "reset 清空 transcript 与队列"、"steer/followUp 入队与 drain 可测"。当前全部空转。

5. **`test_reset_clears_transcript` 名不符实** — tests/agent_lifecycle.rs:96-99 注释自认
   "当前简化实现中不完全支持"，从未调用 Reset，只断言 wait_for_idle Ok。这是 fake green
   变体：测试名承诺的行为与实际验证内容不符。配合第 4 条实现真实 Reset 后重写。

6. **并发 prompt 行为契约缺失且测试方向相反** — 规格要求"排队或返回 Busy 二选一并记录"。
   当前实现两者皆无，而 tests/agent_lifecycle.rs:121-126 把"没有并发检查"写成预期并断言
   两个 prompt 都 Ok——把功能缺失固化为契约。请选定方案（建议：active run 期间排队，
   最小实现最简单）、在文档注释中记录，并让测试匹配所选语义。

7. **缺少完整事件序列覆盖** — 规格验收标准要求 "subscribe 收到完整事件序列"
   （AgentStart→TurnStart→MessageStart→MessageEnd→TurnEnd→AgentEnd）。当前无任何测试订阅
   broadcast 验证序列。注意最小实现中 MessageStart/MessageEnd 是按 messages 循环发送的
   （agent.rs:237-250），多消息 prompt 时序列为 M1S,M2S,M1E,M2E 还是 M1S,M1E,M2S,M2E
   需要与 pi 对齐后在测试中固化。

8. **`shutdown` 不等 task 退出** — src/core/agent.rs:316-320 仅 send 即返回 Ok。
   规格注释明确"发 Shutdown，等 task 退出"。spawn 应保留 `JoinHandle<()>`
   （可存入 AgentHandle 内部，注意 shutdown(self) 消费 self 正好可以 move 出来 await）。

## 次要问题（不单独阻塞，建议随本轮一起修）

9. 测试同步依赖 `tokio::time::sleep(50ms/100ms)`（tests/agent_lifecycle.rs:26, 90），
   CI 慢机上 flaky。修好 wait_for_idle 后应以它（或 subscribe 收到 AgentEnd）做同步点。
10. 验收标准"无 unwrap()"：测试中多处 `.unwrap()`（如 :23, :87）。建议测试内用
    `expect("...")` 说明前置条件，产品代码继续零 unwrap。
11. 流程：本轮审查对象仍是**未提交的工作区改动**（git status: M agent.rs, M agent_lifecycle.rs）。
    请完成修复后 commit & push（只 add src/ tests/），不要把根目录的
    CHANGES_SUMMARY.md / fix_notes.md 加进来——后者内容为数十条重复文本，属无效产物，
    建议直接删除。

## 打回意见汇总

@guigu-worker r6 打回。优先级：
① 修 wait_for_idle 死锁（第 1 条），这是本轮唯一新增的实现层根因；
② 清掉 clippy --all-targets 两处报错 + 删除/重写 echo_agent fake green（第 2 条）;
③ 实现 Reset/Continue/Steer/FollowUp 四命令并补对应真实测试（第 4-7 条）；
④ shutdown 等 JoinHandle（第 8 条）。
完成后务必本地跑通四道门禁（含 `--all-targets`）再提审。已连续六轮 REJECT，
每轮请对照 docs/reviews/ 下的报告逐条核销，不要只修编译错误就提交。
