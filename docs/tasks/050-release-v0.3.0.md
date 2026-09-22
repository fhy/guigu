# Task 050: 发布 v0.3.0（十二~十六期正确性/稳健性修复）

## Background

PM 指令「发布小版本，推送 crates」。当前 crates.io 上 `guigu v0.2.0`（tag `v0.2.0`，2026-09-13，039 发布快照）之后，十二~十六期（040–049）累计完成 62 个 commit 的正确性修复与代码卫生改进，均未重新发布到 crates.io。

自 v0.2.0 的变更性质：**无新 feature、无新用户可见功能开关**，全部为正确性修复（bug fix）+ 内部代码质量收尾，但**引入了 additive 的公开 API 表面**：

- `ProviderError` 新增 `Aborted` / `Timeout` 变体（040）
- `LoopConfig` 新增 `request_timeout` 字段（040）
- `CompactionPolicy` 新增 `reserve_output_tokens` / `protocol_wrapper_tokens` 字段（043）

因此本任务 = 发布新版本，把当前 HEAD 全量修复与改进打包发布。

## Goal

发布 `v0.3.0`：版本号 minor bump `0.2.0 → 0.3.0`，把十二~十六期（040–049）全部变更打包到 crates.io；同步更新 CHANGELOG 与 README 版本号。

## Design Notes

### 1. 版本号：`0.2.0 → 0.3.0`（minor bump）

理由（三点叠加）：

1. **PM 明确指令「小版本」** = minor bump。
2. **公开 API 表面 additive 变更**：`ProviderError` 增变体对下游 exhaustive match 是潜在破坏；`CompactionPolicy`/`LoopConfig` 增字段是 additive。按 0.x 语义，cargo 视 `0.2 → 0.3` 为不兼容需显式 opt-in，minor bump 是**保守安全**选择——正确表达「存在公开契约增量」。
3. 与本仓库既有约定一致（039：0.x minor 表达「有意义的增量」）。

### 2. 自 v0.2.0 的变更总览（CHANGELOG 素材，按能力分组）

- **Runtime 正确性（040）**：`tool_call` 截断保护——超长批量整批失败不执行、合成错误 `ToolResult` 入 transcript；建流取消/超时——`select!` 竞争取消 + `ProviderError::Aborted`/`Timeout` + `LoopConfig::request_timeout`。
- **上下文安全（041）**：压缩提交语义——`PreparedContext { request_messages, commit }` 解耦「请求投影」与「提交」，仅 `commit: Some` 时改写权威 transcript 并持久化，失败/取消不改写；截断升级为 turn/user boundary 粒度，绝不产出孤立 ToolResult。
- **重试与限速（042）**：`ProviderError` 重试分类（`Transient` / `Permanent` / `RateLimited`），解析 `Retry-After`（HTTP-date + 秒两种形式）。
- **上下文预算精确化（043）**：预算公式 `available = context_window − reserve_output_tokens` 再扣固定开销；消息估算优先取 transcript 末条 `AssistantMessage.usage.input` 基线 + 增量 `chars/4`，无 usage 回退全量 `chars/4`。
- **代码卫生（044–049）**：Retry-After 解析双 adapter 去重、上下文预算 API 文档/注释校准、runtime_loop 测试拆分、tests/common helper 去重、冗余 re-export 清理、文档注释残留收尾。

### 3. 交付物（阶段 A 准备，Developer 执行）

1. **Cargo.toml**：`version = "0.3.0"`（唯一改动点；`[features]` 与 optional 依赖声明均已在 HEAD 定稿，`httpdate` 已于 042 加入，**无需改动**）。
2. **CHANGELOG.md**：新增 `## [0.3.0] - <发布日期>` 段，置于 `[0.2.0]` 之上，按上节 5 组覆盖 040–049 用户可见能力。
3. **README.md**：`guigu = "0.2.0"` → `"0.3.0"`（两处：Installation dependencies 第 32 行、default-features 示例第 49 行）。feature flags 表无需改动（feature 集合自 v0.2.0 未变）。

### 4. 发布步骤（Developer 执行，分两阶段）

**阶段 A — 准备（无凭证亦可完成，DoD 门槛）**

1. 改版本号 + CHANGELOG + README，commit。
2. 全 feature 矩阵门禁（必须全绿）：
   - `cargo check --all-features`
   - `cargo check --no-default-features`
   - `cargo test --all-targets`（default features）
   - `cargo test --all-features`
   - `cargo clippy --all-targets --all-features -D warnings`
   - `cargo fmt --check`
3. `cargo package --list` 校验包内容：含 `src/`、`tests/`、`README.md`、`LICENSE`、`CHANGELOG.md`、`Cargo.toml`，不含 `target/`、`.git/`、`docs/`、`.opencode/`。

**阶段 B — 发布（需 crates.io 凭证 + PM 决策，独立于阶段 A）**

4. `cargo publish`（或先 `cargo publish --dry-run` 验证）。
5. 打 annotated tag `v0.3.0` 并 `git push origin v0.3.0`。
6. 验证：`cargo install guigu --version 0.3.0 --force` 成功，`guigu --version` → `0.3.0`。

### 5. 错误处理

- `cargo publish` 报凭证/已存在版本错误 → 停止，报 PM（凭证或版本号需 PM 决策）。
- feature 矩阵任一门禁红 → 修复后重跑，不得带红发布。
- `cargo package --list` 出现多余/缺失文件 → 修正 `include` 白名单后重跑（参照 019 的 `include` 锚定教训）。

### 6. 边界声明

- **不改 `src/` `tests/` 逻辑**；仅 `Cargo.toml` version 字段 + 根文档（CHANGELOG/README）。
- **不引入新依赖、不改 feature 集合**（均已在 HEAD 定稿）。
- `docs/` 内部任务文档本任务不动。
- 实际 `cargo publish` 需 crates.io 凭证 + PM 授权，作为阶段 B 独立决策，不阻塞阶段 A。
- 根文件（Cargo.toml/CHANGELOG/README）归属沿用 039 发布任务先例：Developer 执行。

## Files

- Cargo.toml（仅 `version` 字段）
- CHANGELOG.md（新增 `[0.3.0]` 段）
- README.md（两处版本号 `0.2.0` → `0.3.0`）

## Acceptance Criteria

- [ ] `Cargo.toml` `version = "0.3.0"`；`[features]` 段与 optional 依赖与 HEAD 一致、无新增/删除
- [ ] CHANGELOG.md 新增 `[0.3.0]` 段，覆盖 040–049（5 组：runtime 正确性 / 上下文安全 / 重试限速 / 预算精确化 / 代码卫生）
- [ ] README.md 两处版本号改为 `0.3.0`；feature flags 表不变
- [ ] `cargo check --all-features` passes
- [ ] `cargo check --no-default-features` passes
- [ ] `cargo test --all-targets` passes
- [ ] `cargo test --all-features` passes
- [ ] `cargo clippy --all-targets --all-features -D warnings` passes
- [ ] `cargo fmt --check` passes
- [ ] `cargo package --list` 含必要文件、不含 `target/` `.git/` `docs/` `.opencode/`
- [ ] （阶段 B，PM 决策）`cargo publish` 成功；`cargo install guigu --version 0.3.0 --force` 成功；tag `v0.3.0` 存在

## 修订记录

- v1.0（2026-09-22，Architect）：初稿。依据 PM「发布小版本」指令，版本 minor bump `0.2.0 → 0.3.0`；覆盖十二~十六期（040–049）正确性/稳健性修复与代码卫生。分「准备（阶段 A）」与「发布（阶段 B，需凭证+PM 决策）」两阶段，沿用 039 发布先例。
