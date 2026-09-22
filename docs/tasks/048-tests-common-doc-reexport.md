# Task 048: tests/common 文档注释补回 + 冗余 re-export 清理

## Background

047（tests/common helper 去重）已 PASS，但 Reviewer 在 047-review-r1.md 登记两条非阻塞建议（均纯测试、零产品行为变化）：

1. **文档注释丢失**：合并时丢弃了旧 `tests/common/mod.rs` 与 `tests/common/runtime_loop_fixtures.rs` 上的 `///` 文档注释（如「脚本化 provider：…」「纯文本 turn 脚本：[TextDelta, Done]。」）。参考行号：`provider.rs:11` / `tools.rs:8,35` / `fixtures.rs:12,31,35,64,68,113,121,142,151,160,173,200`。
   - 属文档质量回归，非功能缺陷；046/047 皆因丢注释立项，与 conventions「Public APIs require `///` doc comments」既有风格不一致。
2. **冗余 re-export**：`tests/common/mod.rs:10-21` 额外 `pub use` 了 `guigu::Agent`、`AgentHandle`、`Message`、`Arc`、`AtomicUsize`、`Ordering`、`Duration`、`oneshot` 等 std/guigu 类型。
   - 047 Design Notes 2/3 原意是「helper re-export + 各测试文件自行补 `use`」，且四个 runtime_loop 文件确已各自补 `use std::sync::Arc;` 等，故这些类型 re-export 冗余，额外扩大 `common` 对外耦合面。
   - 另 `#![allow(unused_imports)]`（`mod.rs:3`）因 `pub use` 本身不触发 unused_imports，可能已无必要，并会掩盖未来真实未用导入。

## Goal

- 补回 helper 的 `///` 文档注释，与 parent 版本逐条对应。
- 清理 `tests/common/mod.rs` 冗余 std/guigu 类型 re-export，仅保留 helper 与测试确实依赖的类型。
- 复核并移除已无必要的 `#![allow(unused_imports)]`。

## Design Notes

### 1. 文档注释补回（唯一事实源 = parent 版本）

- 以 047 合并前的旧 `tests/common/mod.rs` 与 `tests/common/runtime_loop_fixtures.rs` 为权威，将各 helper 的 `///` 注释逐条搬回拆分后子模块（`provider.rs` / `tools.rs` / `fixtures.rs`）的对应项。
- 位置清单以 047-review-r1.md 建议 1 的行号为准（上列），Developer 以实际代码与 git parent 为准核对。
- **零行为变化**：只增注释，不改任何 `#[test]` 断言/参数/setup、不改任何函数签名与字段。

### 2. re-export 清理

- 判定标准：`cargo check --all-targets` / `cargo test --all-targets` 全绿。
- 若某 std/guigu 类型确被其它测试 crate 仅靠 `use common::*` 取用，则保留该条（避免破坏既有 pub 路径，对齐 047 AC「其它 crate 既有 pub 路径零破坏」）；否则移除。
- 不得因此改动任何测试用例逻辑（不新增/删除/改写 `#[test]`）。

### 3. `#![allow(unused_imports)]` 复核

- 移除该 allow 后跑 `cargo clippy --all-targets --all-features -- -D warnings`：
  - 若仍 0 warning → 确认已无必要，永久删除。
  - 若触发 unused_imports → 说明仍有真实需要，恢复该 allow（并保留）。

## Files

- `tests/common/mod.rs`（re-export 清理 + allow 复核）
- `tests/common/provider.rs`（补回 `///`）
- `tests/common/tools.rs`（补回 `///`）
- `tests/common/fixtures.rs`（补回 `///`）

## Acceptance Criteria

- [ ] cargo check --all-targets passes
- [ ] cargo clippy --all-targets --all-features -- -D warnings passes（0 warning）
- [ ] cargo test --all-targets passes（通过总数不变；其它集成测试 crate 仍全部编译+通过）
- [ ] cargo fmt --check passes
- [ ] helper `///` 文档注释已补回（与 parent 版本逐条对应，位置覆盖 047-review-r1 建议 1 清单）
- [ ] 冗余 std/guigu 类型 re-export 已清理，仅保留测试确实依赖的
- [ ] `#![allow(unused_imports)]` 已按 Design Notes 3 复核（移除或保留均以 clippy 为准，不掩盖真实警告）
- [ ] 无既有 `#[test]` 被删除或断言弱化；`assert!`/`assert_eq!`/`assert_ne!` 宏总数不减少
- [ ] 零 `src/` 产品代码变更（本任务只动 `tests/`）

## 修订记录

- v1.0（2026-09-22，Architect）：依据 047-review-r1 非阻塞建议 1/2 立项。纯测试文档注释补回 + re-export/allow 清理，零产品行为变化。
