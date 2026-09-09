# Task 022 Review - Round 1

## 基本信息

- 审查时间: 2026-09-08
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/022-custom-models.md
- 提交: 8e7a4db

## 门禁结果

- cargo check: ✓
- cargo clippy --all-targets -D warnings: ✓（默认 features）
- cargo test --all-targets: ✓（319 个 lib 测试，集成测试全部通过）
- cargo fmt --check: ✓
- feature 验证: `cargo test --no-default-features` ✓，`cargo check --features acp-sse` ✓
- `cargo clippy --all-targets --no-default-features -- -D warnings`: ✗（2 个 unused-imports 错误）

## 代码审查

### 问题

1. **[Critical] `src/config.rs:12`、`src/config/tests.rs:4` — no-default-features 下 clippy 门禁失败**
   - 影响：Task 022 明确要求 `cargo test --no-default-features` 剥离配置解析；同时项目 DoD 要求 clippy 使用 `-D warnings`。当前 `Path`/`PathBuf` 仅被 `config` feature 下的实现或测试使用，关闭 feature 后产生 unused imports，导致严格 clippy 失败。因此 feature 矩阵并未完整通过验收。
   - 建议：将 `src/config.rs` 的路径导入放入 `#[cfg(feature = "config")]` 条件下；将 `src/config/tests.rs` 中仅用于 Config 测试的路径导入及相关测试整体按 feature 条件隔离（或分别条件导入）。修复后重新运行 `cargo clippy --all-targets --no-default-features -- -D warnings` 和 `cargo test --no-default-features`。

2. **[Warning] `src/config.rs:20-23` — `ModelConfig` 未按规格派生 `PartialEq, Eq`**
   - 影响：规格设计明确给出 `#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]`。当前公开配置类型无法直接比较，降低嵌入方、调用方和测试对配置值进行比较/缓存的便利性，属于公开 API 与已批准规格不一致。
   - 建议：为 `ModelConfig` 补充 `PartialEq, Eq` 派生；同时补充一个相应断言测试，确认派生行为可用。

### 建议

1. `src/config.rs:128-130`：`UnknownProtocol` 目前没有实际构造路径；未知 TOML/serde 协议会被统一映射为 `ProviderConfigError::Parse`。若保留该公开错误变体，建议在解析层显式区分未知协议，或删除该未使用变体并同步规格/文档，避免 API 表达与实际错误映射不一致。
2. `tests/config.rs:36-37、52-53`：`unwrap`/`expect` 出现在集成测试中，产品代码未发现新增 unwrap；这不阻塞本任务，但可继续遵循项目测试错误上下文规范。

## 结论

- [ ] 通过
- [x] 打回

## 下一步

- @guigu-worker 请修复问题 1（必须）：修复 no-default-features 严格 clippy 失败。
- 建议同步修复问题 2，使公开数据结构完全符合 Task 022 v1.1 规格。
