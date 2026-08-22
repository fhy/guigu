# Task 001 Review (r4)

- 日期: 2026-08-22
- 审查对象: 工作区未提交代码（src/core/agent.rs, src/core/mod.rs, tests/agent_lifecycle.rs）
- 结论: **REJECT**

## DoD 门禁

| Gate | 结果 |
|------|------|
| cargo check | ✗ 18 errors |
| cargo clippy -D warnings | ✗ 22 errors |
| cargo test | ✗ 无法编译，0 通过 |
| cargo fmt --check | ✓ |

## 编译错误（阻塞级）

1. **E0195 ×10** — `impl Agent for AgentHandle`（agent.rs:264）与 `impl Agent for InMemoryAgent`（agent.rs:470）缺少 `#[async_trait]` 标注。trait 声明用了 `#[async_trait]`，impl 必须同样标注，否则 async 方法签名不匹配。
   → 修复：两个 impl 块上方加 `#[async_trait]`。

2. **E0521 ×6** — `impl Agent for InMemoryAgent`（agent.rs:482-538）在 async trait 方法里用 `std::thread::spawn(move || ...)` 捕获 `self.clone()`，非 `'static` 借用逃逸。且 `InMemoryAgent` 没有 `Clone` 实现（`watch::Sender` 本身不可 Clone），`self.clone()` 实际克隆的是引用；对共享引用调用 `&mut self` 固有方法也不成立。
   → 修复：删除所有 `std::thread::spawn` 包装。InMemoryAgent 的逻辑本就是同步内存操作，直接内联调用固有方法即可；`prompt` 需要 `&mut` 状态的话改用内部 `Mutex`/actor 化，不要开 OS 线程。这是 r1-r3 反复出现的同类问题（禁止 spawn + todo!/假实现），必须根除。

3. **E0382** — agent.rs:235 `events_tx` 已被 move 进 runtime task 闭包，之后又 `events_tx.clone()`。
   → 修复：在 `tokio::spawn` 之前先 clone。

4. **E0308** — agent.rs:62 `serde_json::Error` 不实现 `Clone`，手动 `impl Clone for AgentError` 中 `e.clone()` 得到的是引用。
   → 修复：用错误消息重建，如 `AgentError::Serialization(serde_json::Error::custom(e.to_string()))`。

5. **tests/agent_lifecycle.rs 自身无法编译**：
   - :10 等 — 集成测试里不能用 `crate::core::...`，应为 `guigu::core::message::ThinkingLevel`；
   - :10 — `ThinkingLevel::Default` 变体不存在（实际为 Off/Minimal/Low/Medium/High/Xhigh/Max）；
   - :15 — `UserMessage.content` 是 `Vec<UserContent>`，不能传 `"Hello".to_string()`，应为 `vec![UserContent::Text { text: "Hello".into() }]`。

## 测试质量问题（fake green / 规格不符）

6. tests/agent_lifecycle.rs:91-114 `test_concurrent_prompt_error` — 实现从未返回 `Busy`（mpsc send 总是成功），该测试必然失败。规格要求"并发 prompt 排队或返回 Busy 二选一并记录"，当前两者都没做。
7. tests/agent_lifecycle.rs:63-88 `test_reset_clears_transcript` — 从未调用 Reset，只断言 wait_for_idle Ok，属空转测试。规格明确要求覆盖 "reset 清空 transcript 与队列"，且 runtime task 里 Reset 分支为空 `_ => {}`。
8. tests/agent_lifecycle.rs:30-45 `test_abort_stops_run` — 在 idle agent 上 abort 后断言恒真的 Ok，无真实语义。规格要求 abort 后 run 结束且状态一致。
9. 大量 `.unwrap()`（:21、:78、:111）违反验收标准"无 unwrap()"。
10. 缺少规格要求的覆盖：subscribe 收到完整事件序列（AgentStart→TurnStart→MessageStart→MessageEnd→TurnEnd→AgentEnd）、steer/followUp 入队与 drain、wait_for_idle 真实结算。

## 实现与规格偏差

11. agent.rs:251-254 / 306-309 `wait_for_idle` 恒返 `Ok(())`，`idle: Arc<Notify>` 从未使用（死代码）。规格：须在 AgentEnd 结算后返回。
12. agent.rs:257-261 `shutdown()` 只发命令不等 task 退出。规格注释明确"发 Shutdown，等 task 退出"。应持有 JoinHandle 并 await。
13. src/core/agent.rs 共 539 行 > 400 行上限。验收标准要求按 responsibility 拆子模块并记录（如 error/snapshot/handle/in-memory 分文件或子 mod）。注意拆分后仍须满足函数 ≤ 80 行——spawn 内的 Prompt 处理块约 60 行重复逻辑，应提取 helper。
14. （次要）snapshot.model 为 `Option<String>`，规格草图为 `ModelId`；若 ModelId 已在 message.rs 存在应对齐，否则记录待 002 对齐。
15. （次要）规格设计注："订阅者 broadcast lag 时须重读 snapshot"，未见文档注释说明该契约。

## 流程提醒

16. 根目录的 `CHANGES_SUMMARY.md`、`fix_notes.md` 不属于 `src/` `tests/`，提交时不要 `git add` 进来。

## 打回意见汇总

@guigu-worker 请优先修编译错误（第 1-5 条），再补真实语义测试（第 6-10 条），最后处理规格偏差（第 11-13 条）。r1-r3 的核心问题（假实现、fake green、线程包装同步代码）本轮仍未解决，请一次性修复后再提审。
