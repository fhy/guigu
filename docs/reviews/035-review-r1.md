# Task 035 Review - Round 1

## 基本信息

- 审查时间: 2026-09-12
- 审查员: guigu-reviewer
- 任务规格: `docs/tasks/035-deps-lockfile-ci.md`
- 审查提交: `ec920ef`

## 门禁结果

- cargo check: ✓
- cargo clippy --all-targets -- -D warnings: ✓
- cargo test --all-targets: ✓（367 库测试、18 binary 测试及全部集成测试通过）
- cargo fmt --check: ✓
- cargo test --no-default-features: ✓（261 测试通过）
- cargo clippy --no-default-features --all-targets -- -D warnings: ✗（1 个错误）
- cargo test --features acp-sse: ✓
- cargo test --features tui: ✓
- `bash scripts/package-check.sh`: ✓

## 代码审查

### 问题

1. [Critical] `src/core/tool.rs:121` — `schema` feature 被关闭时，`tool_parameters<T>` 的泛型参数未被使用，导致严格 clippy 失败。
   - 影响: Task 035 要求全 feature 矩阵门禁；`cargo clippy --no-default-features --all-targets -- -D warnings` 无法通过，CI/发布验收不完整。该问题也会阻断嵌入方使用 `default-features = false` 的构建质量门禁。
   - 建议: 在 `#[cfg(not(feature = "schema"))]` 分支保留类型参数的可检查使用，例如在函数体中加入 `let _ = std::marker::PhantomData::<T>;`，或采用等价的零成本 `PhantomData` 表达式；然后重新运行 no-default-features clippy 与完整门禁。不要通过全局 `allow` 掩盖该 warning。

### 已核验内容

- `Cargo.lock` 已将 `chacha20` 从 `0.10.1` 更新为 `0.10.2`。
- `.github/workflows/ci.yml` 在干净 checkout 中调用 `scripts/package-check.sh`。
- `scripts/package-check.sh` 对 `cargo package --list` 失败返回非零，并拒绝 `docs/`、`.git/`、`target/`、`.opencode/` 路径；本地执行通过。

## 结论

- [ ] 通过
- [x] 打回

## 下一步

@guigu-worker 请修复上述 Critical：补齐 `--no-default-features` 下 `tool_parameters<T>` 的泛型使用，运行并确认全 feature 矩阵门禁后重新提交审查。
