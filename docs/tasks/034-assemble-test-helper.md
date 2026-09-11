# Task 034: 装配测试样板提炼 helper

## Background
026 r1 非阻塞建议 1（docs/reviews/026-review-r1.md）：`src/bin/guigu/assemble.rs:307-375` 测试中重复构造 CLI、创建 session 和 shutdown 流程，可提取测试 helper 避免样板扩大。

## Goal
提取测试 helper，收敛装配测试中的重复构造/清理样板；不改产品逻辑。

## Design Notes
- 纯测试内部重构；提取「构造 CLI 参数 + 创建 session + shutdown 清理」为共享 helper（`#[cfg(test)]` 内）。
- 不改任何产品代码路径；保持既有断言语义等价。

## Files
- src/bin/guigu/assemble.rs（测试模块内提取 helper）

## 错误处理
无新错误类型；helper 内清理逻辑需可靠（避免 panic 泄漏资源）。

## 测试要求
- 既有装配测试（自定义/默认 system prompt 注入、base_url 透传等）全部保持通过。

## Acceptance Criteria
- [ ] cargo check
- [ ] cargo clippy --all-targets -- -D warnings
- [ ] cargo test --all-targets
- [ ] cargo fmt --check
