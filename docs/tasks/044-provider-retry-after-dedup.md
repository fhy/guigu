# Task 044: 适配器 Retry-After 解析去重 + 代码卫生

## Background

042 定稿并四绿通过，但 Reviewer 在 r4 报告（`docs/reviews/042-review-r4.md`）中登记了两条非阻塞技术债，随 042/043 收尾批次一并清理：

1. **[Warning] `parse_retry_after` 逐字重复**：`src/adapters/openai/mod.rs` 与 `src/adapters/anthropic/mod.rs` 中各自维护了一份完全相同的 `Retry-After` 头解析实现（秒数 + HTTP-date 两种格式）。重复代码分散在两个 provider adapter 中，后续任一语义修正（如封顶、格式扩展）需同步改两处，易漏改漂移。
2. **[Note] 完全限定路径冗余**：`src/core/runtime/turn.rs` 使用 `crate::core::provider::RetryClass::Permanent` 完全限定路径，顶部未 `use` 导入 `RetryClass`，与仓库既有导入风格不一致。

## Goal

- 将 `parse_retry_after` 抽取为 `src/adapters/` 下的单点共享实现，两个 adapter 统一调用，消除逐字重复。
- 收敛 `turn.rs` 的 `RetryClass` 为顶部 `use` 导入 + 短路径引用。
- **严格零行为变化**：解析语义、错误分类、重试/退避逻辑、公开 API 均不得改变。

## Design Notes

### 1. `parse_retry_after` 抽取（042 r4 建议 1）

- 新增文件 `src/adapters/retry_after.rs`，承载唯一的 `parse_retry_after` 实现。
- `src/adapters/mod.rs` 需声明子模块（`mod retry_after;`）。
- **签名与语义冻结**：以现有两处实现中任一为准，抽取后**保持原签名与原解析语义不变**，仅做搬运去重，不优化、不改写、不扩展。具体冻结点：
  - 输入：HTTP `Retry-After` 头原始值（沿用既有入参类型，如 `Option<&str>` / `Option<&HeaderValue>`，以现有实现为准）。
  - 支持格式：① 纯整数秒数；② HTTP-date（RFC 7231）。两种格式的既有处理逻辑原样保留。
  - 输出：解析成功 → `Some(Duration)`；值缺失或非法 → `None`。
- 可见性建议 `pub(crate)`（仅 adapters 内部使用），具体可见性由 Developer 依据调用方决定，但**不得导出到 crate 公开面**（不改 `lib.rs` 公开导出）。
- 两处调用点（openai / anthropic）删除本地重复实现，改为引用共享函数。调用点处已有的头读取/传参逻辑保持不变，仅替换实现来源。

### 2. `RetryClass` 导入收敛（042 r4 建议 2）

- `src/core/runtime/turn.rs` 顶部 `use` 区新增 `RetryClass` 导入（与既有 `crate::core::provider::*` 导入风格一致）。
- 将 `crate::core::provider::RetryClass::Permanent` 完全限定路径替换为短路径 `RetryClass::Permanent`。
- 若 `turn.rs` 中还有其它 `RetryClass` 完全限定引用，一并收敛；语义不变。

### 3. 零行为变化约束

- 本任务是纯重构 + 风格收敛，**不新增/删除/修改任何运行时行为**。
- `RetryClass` 分类映射、`retry_class()`/`retry_after()` 判定、runtime 重试循环、adapter 响应转换逻辑均不得改动。
- 042 的既有 wiremock 端到端用例（429 + `Retry-After: 5`、401 分类）必须原样通过，作为回归保障。

## Files

- `src/adapters/retry_after.rs`（新增：`parse_retry_after` 单点实现）
- `src/adapters/mod.rs`（声明 `mod retry_after;`）
- `src/adapters/openai/mod.rs`（删除本地重复实现，改引用共享函数）
- `src/adapters/anthropic/mod.rs`（删除本地重复实现，改引用共享函数）
- `src/core/runtime/turn.rs`（`RetryClass` 顶部导入 + 短路径）

## Acceptance Criteria

- [ ] cargo check --all-targets passes
- [ ] cargo clippy --all-targets --all-features -- -D warnings passes（0 warning）
- [ ] cargo test --all-targets passes（含 042 既有 `tests/adapters.rs` wiremock 用例与 `tests/runtime_loop.rs` 重试用例全绿）
- [ ] cargo fmt --check passes
- [ ] `parse_retry_after` 实现全局唯一：`grep` 确认 openai/anthropic 两目录内不再存在逐字重复的解析实现，仅保留对共享函数的调用
- [ ] 解析语义零变化：秒数、HTTP-date、缺失/非法 → `None` 三类用例（既有测试或新增单测）断言行为与抽取前一致
- [ ] `turn.rs` 不再出现 `crate::core::provider::RetryClass::` 完全限定路径，改为 `use` 导入短路径
- [ ] 产品代码无新增 `unwrap()`；单文件 ≤ 400 行（`retry_after.rs` 新增文件亦须 ≤ 400 行）

## 修订记录

- v1.0（2026-09-22，Architect）：依据 042 r4 建议 1（`parse_retry_after` 逐字重复 → 抽 `src/adapters/retry_after.rs` 单点实现）与建议 2（`turn.rs` 完全限定路径 → `use` 导入）立项。零行为变化纯重构。
