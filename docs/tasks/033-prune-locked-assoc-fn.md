# Task 033: prune_locked 改关联函数

## Background
017-c r2 非阻塞建议（docs/reviews/017-c-review-r2.md）：`src/tools/file_mutation_queue.rs:83` 的 `prune_locked` 接收 `&self` 但未使用 self，可改为关联函数（无 self 参数）以表达其纯 helper 语义。

## Goal
将 `prune_locked` 由方法（`&self`）改为关联函数（无 self），调用点相应调整。

## Design Notes
- 纯签名调整，零行为变化；`prune_locked` 为内部私有 helper（非 pub），无对外影响。
- 调用点从 `self.prune_locked(...)` 改为 `Self::prune_locked(...)` 或自由函数形式（以实际代码为权威）。

## Files
- src/tools/file_mutation_queue.rs

## 错误处理
无新错误类型。

## 测试要求
- 既有驱逐测试（释放条目 / in-flight 条目 / 自动驱逐 / 驱逐后互斥）全部保持通过。

## Acceptance Criteria
- [ ] cargo check
- [ ] cargo clippy --all-targets -- -D warnings
- [ ] cargo test --all-targets
- [ ] cargo fmt --check
