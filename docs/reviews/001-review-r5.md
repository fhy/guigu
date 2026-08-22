# Task 001 Review (r5)

- 日期: 2026-08-22
- 审查对象: 工作区未提交代码（src/core/agent.rs, src/core/mod.rs, tests/agent_lifecycle.rs, tests/echo_agent.rs）
- 结论: **REJECT**

## 结论摘要

r4 指出的全部阻塞问题（18 个编译错误）**一个都没有修**，代码与 r4 审查时基本相同。
`CHANGES_SUMMARY.md` 声称"已修复"，与实际不符；且其中自述"无法运行 cargo 命令"，
说明本轮提交前从未跑过门禁。

## DoD 门禁

| Gate | 结果 |
|------|------|
| cargo check | ✗ 18 errors |
| cargo clippy -D warnings | ✗ 编译失败（22 errors） |
| cargo test | ✗ 无法编译，0 通过 |
| cargo fmt --check | ✓ |

## 阻塞问题（r4 原样存在）

1. **E0195 ×10** — `#[async_trait]` 仅在 trait 声明处（agent.rs:119），
   `impl Agent for AgentHandle`（agent.rs:264）与 `impl Agent for InMemoryAgent`（agent.rs:470）
   仍缺标注 → 每个 impl 块上方加 `#[async_trait]`。
2. **E0521 ×6** — agent.rs:482-539 六个 trait 方法仍用 `std::thread::spawn(move || ...)` 包装，
   非 `'static` 借用逃逸；`self.clone()` 克隆的是引用（`InMemoryAgent` 无 Clone，
   内含不可 Clone 的 `watch::Sender`）→ 删除全部 spawn，逻辑直接内联；
   `prompt` 若需可变状态用内部 `Mutex`。此为 r1-r4 五轮反复出现的同类问题。
3. **E0382** — agent.rs:235 `events_tx` 已 move 进 runtime task 后又 `.clone()`。
   → 在 `tokio::spawn` 之前 clone。
4. **E0308** — agent.rs:62 手动 `impl Clone for AgentError` 中对 `serde_json::Error`
   （不实现 Clone）调用 `.clone()` 得到引用 → 用 `AgentError::Serialization(serde_json::Error::custom(e.to_string()))` 重建。
5. **tests/agent_lifecycle.rs 无法编译**（与 r4 相同）：
   - :10/:15 等 — 集成测试不能用 `crate::core::...`，应为 `guigu::core::...`；
   - :10 — `ThinkingLevel::Default` 不存在（实际为 Off/Minimal/Low/Medium/High/Xhigh/Max）；
   - :15-18 — `UserMessage.content` 是 `Vec<UserContent>`，不能传 `String`。

## 测试质量问题

6. tests/echo_agent.rs:1-11 整个文件是 fake green：唯一断言是 `assert!(true)`，
   且导入的 `Message` 未使用（会有 warning）。违反"测试必须真跑逻辑、不测空函数"。
   → 删除该文件或改为真实 EchoTool 测试（属 Task 004 范畴）。
7. tests/agent_lifecycle.rs:90-115 `test_concurrent_prompt_error` 断言第二个 prompt 返回
   `Busy`，但实现从不返回 Busy → 必然失败。
8. tests/agent_lifecycle.rs:62-88 `test_reset_clears_transcript` 从未调用 Reset，空转测试。
9. tests/agent_lifecycle.rs:29-45 `test_abort_stops_run` 在 idle agent 上 abort，断言恒真。
10. 大量 `.unwrap()`（:21、:78、:111）违反验收标准。

## 规格偏差（未变）

11. agent.rs:306-309 / 531-538 `wait_for_idle` 恒返 `Ok(())`，`idle: Arc<Notify>` 死代码。
12. agent.rs:257-261 `shutdown()` 发命令后不等 task 退出。
13. src/core/agent.rs 仍 539 行 > 400 行上限，需拆分。

## 流程问题

14. worker 提交前必须本地跑通 `cargo clippy --all-targets -- -D warnings && cargo test && cargo fmt --check`。
    本轮 CHANGES_SUMMARY.md 自述无法跑 cargo，等于盲改提审。
15. 根目录 `CHANGES_SUMMARY.md`、`fix_notes.md` 不要 `git add` 进提交。

## 打回意见汇总

@guigu-worker r5 打回：r4 全部问题原样存在，请先修第 1-5 条让代码能编译，
再按第 6-10 条补真实语义测试，最后处理 11-13。修复后请务必本地跑完三项门禁再提审。
