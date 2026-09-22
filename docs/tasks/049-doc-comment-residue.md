# Task 049: 文档注释残留收尾

## Background

十三~十五期（044~048）审查过程中，Reviewer 累计登记三条**非阻塞文档注释残留**，均零行为变化、纯文档质量，且都违背 conventions「Public APIs require `///` doc comments」与项目一贯的文档一致性纪律：

1. **044 r1 建议 1**：`src/adapters/retry_after.rs` 的 `parse_retry_after`（约第 3 行）缺 `///`，未说明支持的 Retry-After 格式与 `None` 返回语义。
2. **045 r1 建议 1**：`with_overhead` 的连续摘要句语义重叠，可合并为一句（`src/core/context.rs`，以实际代码为权威）。
3. **048 r2 残留 [Info]**：`tests/common/provider.rs:88` 的 `HangingProvider` struct `///` 与 parent 源文件（`e9ce9ea^:tests/common/runtime_loop_provider.rs:107-108`）两行版不一致。

## Goal

一次性清理上述三条文档注释残留，保持零行为变化。

## Design Notes

1. **retry_after `///` 补回**：为 `parse_retry_after` 补文档注释，说明——支持解析的 Retry-After 格式（HTTP-date 与 delta-seconds 两种）、以及无法解析时的返回语义（`None`）。具体措辞以函数实际签名为准。
2. **with_overhead 摘要句合并**：将语义重叠的连续摘要句合并为一句，消除冗余，不改动任何字段/常量/签名。
3. **HangingProvider 注释对齐**：将 struct `///` 对齐 parent 两行版原文（参考：`/// 挂起 provider：\`stream()\` 永不返回（\`pending()\` future），用于验证建流阶段的取消/超时（Task 040）。runtime 的 \`select!\` 应在 provider 返回前抢先取消。`），以实际 parent 文件内容为权威逐字核对。

**统一约束**：只增/改注释，不改任何逻辑、函数签名、字段、常量、`#[test]` 断言与参数。

## Files

- `src/adapters/retry_after.rs`（补 `///`）
- `src/core/context.rs`（with_overhead 注释合并）
- `tests/common/provider.rs`（HangingProvider 注释对齐）

## Acceptance Criteria

- [ ] cargo check --all-targets passes
- [ ] cargo clippy --all-targets --all-features -- -D warnings passes（0 warning）
- [ ] cargo test --all-targets passes（通过总数不变）
- [ ] cargo fmt --check passes
- [ ] 仅注释/文档变更，零逻辑/签名/字段/常量/测试断言变更

## 修订记录

- v1.0（2026-09-22，Architect）：依据 044 r1 / 045 r1 / 048 r2 三条非阻塞文档注释残留合并立项。纯文档注释收尾，零行为变化。
