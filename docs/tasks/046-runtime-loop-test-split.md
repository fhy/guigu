# Task 046: runtime_loop 测试拆分 + 预算口径 M4 补强

## Background

042/043 定稿并四绿通过，但 Reviewer 登记了两条测试侧非阻塞技术债，随 042/043 收尾批次一并清理：

1. **042 r4 建议 3 [Note]**：`tests/runtime_loop.rs` 现 1364 行，超过 conventions「单文件 ≤ 400 行」上限。属既有超标（本轮 +74 行，非回归），建议按场景拆分子模块。
2. **043 r4 建议 7 [可选 M4 补强]**：043 冻结 closure matrix 中 M4（`estimate`/`fits`/`truncate` 口径一致）已在 R2 判为非阻塞、R3/R4 以构造性论证闭环，但缺一条「含 `usage.input` 且 `fits==true` ⇒ `truncate` 返回原列表」的**直接断言**，可补强。

## Goal

- 将 `tests/runtime_loop.rs` 按场景拆分为多个 ≤ 400 行的测试文件，**测试总数不减少、断言不弱化、零行为变化**。
- 为 043 M4 口径一致性补一条直接断言用例，锁定「fits==true ⇒ truncate 不截断」。

## Design Notes

### 1. `tests/runtime_loop.rs` 场景化拆分（042 r4 建议 3）

- 目标：每个拆分后文件 ≤ 400 行（conventions Size Limits）。
- 拆分维度按既有用例场景聚合，建议（以实际既有 `#[test]` 清单为准，Developer 机械搬运，不得改写断言）：
  - `tests/runtime_loop.rs`：保留主循环编排/生命周期核心用例。
  - `tests/runtime_loop_retry.rs`：重试/退避/限流相关用例（含 042 的 `test_retry`、`test_rate_limited_retry_after_is_used`、`test_rate_limited_without_retry_after_uses_exponential_backoff`、`Permanent` 不重试、退避期可取消等）。
  - `tests/runtime_loop_truncate.rs`：长度截断/预算截断相关用例（040/041 引入）。
  - 其余场景（steering / 生命周期 / tool 执行等）按需再拆，直至每个文件 ≤ 400 行。
- 共享 fixture/helper 复用既有 `tests/common/mod.rs`（conventions 集成测试约定）；若拆分引入跨文件共享 helper，放入 `tests/common/`，**不复制粘贴测试逻辑**。
- **严格零行为变化**：仅移动既有 `#[test]` 的位置与所属文件，不改动任何用例的断言、参数、setup；不得删除或合并任何用例。
- 拆分后 `cargo test --all-targets` 的通过用例总数必须与拆分前一致（`runtime_loop` 目标 21 passed 不得减少，除非确有既有冗余用例且经 Reviewer 确认，本任务默认不删用例）。

### 2. M4 直接断言补强（043 r4 建议 7）

- 在 context 测试（`src/core/context/tests.rs` 或既有 context 集成测试文件）新增一条用例，直接锁定 M4：
  - 构造 `ContextBudget::with_overhead(...)` 与一个**含 `usage.input` 基线**的 transcript，使 `fits(msgs) == true`。
  - 断言 `truncate(msgs)` 返回的列表与输入**长度一致（或元素逐一相等）**，即「fits 为真 ⇒ truncate 不截断」。
  - 断言须对「fits/truncate 口径分裂」有判别性：若 `truncate` 错误地二次扣减 `fixed_overhead` 或与 `fits` 口径不一致，本用例应失败。
- 复用 043 既有测试的构造手法（如 `with_overhead(100, ...)` + 含 `usage.input` 的 assistant 消息），断言采用 `assert_eq!` 直接比较列表长度/内容，不得用空断言。

### 3. 边界约束

- 不修改 `src/` 产品代码（本任务只动 `tests/` 与 context 测试模块内的 `#[cfg(test)]`）。
- 不改变任何既有测试的语义强度；M4 补强为**新增**用例，不替换、不弱化既有 043 用例。

## Files

- `tests/runtime_loop.rs`（拆分为主循环核心用例）
- `tests/runtime_loop_retry.rs`（新增：重试/退避/限流场景）
- `tests/runtime_loop_truncate.rs`（新增：长度截断场景）
- `tests/common/mod.rs`（如拆分引入跨文件共享 helper，则扩展）
- `src/core/context/tests.rs` 或 context 相关测试文件（新增 M4 直接断言用例）

## Acceptance Criteria

- [ ] cargo check --all-targets passes
- [ ] cargo clippy --all-targets --all-features -- -D warnings passes（0 warning）
- [ ] cargo test --all-targets passes（通过总数 ≥ 拆分前基线；`core::context` 用例数 +1 的 M4 补强用例真实执行通过）
- [ ] cargo fmt --check passes
- [ ] 每个拆分后的 `tests/runtime_loop*.rs` 文件 ≤ 400 行；无既有 `#[test]` 被删除或断言被弱化
- [ ] 新增 M4 用例：`fits(msgs)==true ⇒ truncate(msgs)` 返回原列表（长度相等/元素相等），且对口径分裂有判别性
- [ ] 测试代码无 `unwrap()` 之外的硬编码外部依赖路径；M4 用例不依赖外部服务
- [ ] 单测试文件 ≤ 30 `#[test]`（conventions 上限）；超则继续按场景拆分

## 修订记录

- v1.0（2026-09-22，Architect）：依据 042 r4 建议 3（`tests/runtime_loop.rs` 1364 行超限 → 按场景拆分）与 043 r4 建议 7（M4 口径一致性补直接断言）立项。测试纯重组 + 补强，零产品行为变化。
