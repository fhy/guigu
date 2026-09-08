# Task 025 Review - Round 1

## 基本信息

- 审查时间: 2026-09-08
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/025-acp-sse-http.md
- 审查提交: 2c90420

## 门禁结果

- cargo check: ✓
- cargo clippy --all-targets -- -D warnings: ✓
- cargo test --all-targets: ✓（296 库测试、10 个 binary 测试及全部集成测试通过）
- cargo fmt --check: ✓
- cargo check --features acp-sse: ✓
- cargo clippy --features acp-sse --all-targets -- -D warnings: ✓
- cargo test --features acp-sse --all-targets: ✓（304 库测试，SSE 集成测试 4/4 通过）
- cargo test --no-default-features: ✗（编译失败）

## 代码审查

### 问题

1. [Critical] `src/bin/guigu/assemble.rs:17`、`Cargo.toml:23-28` — `--no-default-features` 不满足任务验收条件。
   - 影响：任务明确要求 `cargo test --no-default-features` 下 `acp-sse` 模块跳过且构建通过；当前命令在二进制测试编译阶段报 `unresolved import guigu::adapters`，因为 CLI 二进制无条件导入 `guigu::adapters`，而该模块由 `providers-http` feature 门控。虽然该问题不是 SSE handler 本身引入的，但 Task 025 的 feature 矩阵验收仍然失败，不能按规格通过。
   - 建议：为 CLI binary 增加与 `providers-http` 一致的编译门控（例如在 `Cargo.toml` 使用 `required-features = ["providers-http"]`，或将 CLI 的真实 provider 装配拆到 feature-gated 模块并为无 provider 构建提供明确的 fake/错误路径），然后重新执行 `cargo test --no-default-features`，确认 `axum`/`tokio-stream` 未被启用且所有目标可编译。

### 建议

1. `tests/acp_sse/main.rs:76-116` — 多 client 测试目前先完成 client A，再完成 client B；它验证了隔离，但没有覆盖两个 prompt 真正并发执行的时序。建议使用 `tokio::join!` 或分别 spawn 两个 prompt，并断言两条 SSE 流均能独立完成。
2. `tests/acp_sse/main.rs:1-198` — 集成测试中有 `unwrap()`/`expect()`，属于测试代码且当前不会影响产品安全；若项目继续严格执行“无 unwrap”规范，建议改为带上下文的 `expect` 或统一测试 helper 错误返回，避免断言失败信息不一致。

## 结论

- [ ] 通过
- [x] 打回

## 下一步

- @guigu-worker 请修复上述 Critical 问题，至少使 `cargo test --no-default-features` 通过，并补跑四门禁及 `--features acp-sse` 的测试。
- 修复后提交新 commit，申请 Task 025 Round 2 复审。
