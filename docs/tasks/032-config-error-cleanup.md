# Task 032: config UnknownProtocol 变体清理 + 测试 unwrap 清理

## Background
022 r2 非阻塞建议 1 & 2（docs/reviews/022-review-r2.md）：
1. `src/config.rs:118-121` 的 `UnknownProtocol` 错误变体目前无实际构造路径（未知协议由 serde 解析错误统一映射为 `ProviderConfigError::Parse`），公开错误 API 与实际行为不一致。
2. `tests/config.rs` 及相关测试仍使用 `unwrap`/`expect`。

## Goal
（1）消除 `UnknownProtocol` 未使用变体——删除该变体，或在解析层显式将未知协议映射为 `UnknownProtocol`（二选一，见设计决策）；（2）清理 config 相关测试的 `unwrap`/`expect`，改用带上下文的错误处理。

## Design Notes
- 变体决策：优先**删除**未使用变体（零构造路径 = 零行为变化，公开枚举减少一个从未生效的变体）；若 Developer 审查实际代码发现 `UnknownProtocol` 有引用或有语义价值，则改为「解析层显式映射为 `UnknownProtocol`」。以实际代码为权威，二选一后保持公开错误 API 与实际行为一致。
- 测试清理范围：仅限 `tests/config.rs` 及 config 相关测试，不扩大到全仓（避免越界）；`unwrap`/`expect` 改为 `expect` 带语义说明，或返回 `Result` 用 `?`/`assert`，遵守 conventions「无 unwrap()」规范。

## Files
- src/config.rs（UnknownProtocol 变体处理）
- tests/config.rs（unwrap/expect 清理）

## 错误处理
涉及 `ProviderConfigError` 枚举：若删除变体，确认无 `match` 穷尽分支受影响；若映射，补齐构造路径。

## 测试要求
- 覆盖协议解析路径（已知协议成功 / 未知协议走 Parse 或 UnknownProtocol 语义）。
- 清理后的 config 测试保持等价断言，四门禁全绿。

## Acceptance Criteria
- [ ] cargo check
- [ ] cargo clippy --all-targets -- -D warnings
- [ ] cargo test --all-targets
- [ ] cargo test --no-default-features（若 config feature 可剥离）
- [ ] cargo fmt --check
