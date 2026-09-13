# Task 039: 发布 v0.2.0（补齐九~十一期增量能力，含 `tui` feature）

## Background

用户执行 `cargo install guigu --features tui` 报错：

```
Installing guigu v0.1.0
error: the package 'guigu' does not contain this feature: tui
```

根因已确认（证据链见 Design Notes）：

- crates.io 上 `guigu v0.1.0` 由 git tag `v0.1.0`（commit `a7bcbcf`，2026-09-06，六期/019 发布快照）发布。
- `tui` feature 于九期/023（commit `aeabe14`）才加入，晚于该 tag。
- 自 v0.1.0 之后，九~十一期累计新增：`config`（022）、`tui`（023）、`acp-sse` 实装（025）、`schema`（027）、跨进程文件锁（028）、Agent 插件（029）、system prompt/base-url（026）、持久化 lane head（024）等能力，**均未重新发布到 crates.io**。
- 本地 HEAD 的 `Cargo.toml` 已正确声明 `tui = ["dep:ratatui", "dep:crossterm"]`，代码无缺陷；问题纯粹是「发布物落后于源码」。

因此：不是代码 bug，是**发布/版本管理缺口**。修复方式 = 发布新版本，把当前 HEAD 全量能力打包发布。

## Goal

发布 `v0.2.0`：版本号 minor bump，补齐九~十一期全部增量能力到 crates.io，使 `cargo install guigu --features tui`（及 `--features acp-sse` 等）可用；同步更新 CHANGELOG 与 README feature flags 表。

## Design Notes

### 1. 版本号：`0.1.0` → `0.2.0`（minor bump）

理由：自 v0.1.0 后所有变更均为**增量新增**（新 feature、新 optional 依赖、新工具/插件能力），无破坏性变更（Trait 契约零破坏，见 027/029 规格）。SemVer 语义下 0.x 的 minor bump 正确表达「新增能力」。

### 2. 当前 feature 全景（发布时需完整、正确）

| Feature | Default | 依赖 | 说明 |
|---------|---------|------|------|
| `providers-http` | ✅ | `reqwest` | OpenAI/Anthropic 适配器 |
| `config` | ✅ | `toml` | TOML 配置 + 自定义模型（022） |
| `schema` | ✅ | `schemars` | 强类型工具参数 schema（027） |
| `tui` | — | `ratatui` + `crossterm` | 全屏 TUI（023） |
| `acp-sse` | — | `axum` + `tokio-stream` | ACP SSE/HTTP 远程多 client（025） |

`default = ["providers-http", "config", "schema"]`（已在 HEAD `Cargo.toml` 定稿，无需改动）。

### 3. 交付物（发布前准备，Developer 执行）

1. **Cargo.toml**：`version = "0.2.0"`（唯一改动点；feature 声明保持不变）。
2. **CHANGELOG.md**：新增 `## [0.2.0] - <日期>` 段，覆盖 022–029 用户可见能力，分组：
   - 配置（022 TOML + 自定义模型；026 system prompt + base-url）
   - TUI（023 全屏 TUI）
   - 会话（024 持久化 lane head / 活动分支指针）
   - 远程与协议（025 ACP SSE/HTTP 远程多 client）
   - 工具（027 schemars 强类型参数 schema）
   - 并发与锁（028 跨进程会话锁 / 文件锁）
   - 扩展（029 Agent 插件 / 生命周期钩子）
   - 维护（030–038 锁纪律/去 unwrap/CI 校验等，一句话概括）
3. **README.md**：
   - `guigu = "0.1.0"` → `"0.2.0"`（三处：Installation 的 dependencies、default-features 示例）。
   - feature flags 表替换为上方完整 5 行表（当前只列了 `providers-http` + `acp-sse` 存根，缺 `config`/`schema`/`tui`，且 `acp-sse` 已非存根）。
   - Features 小节补一句 TUI、schemars、Agent 插件、跨进程锁能力。

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
3. `cargo package --list` 校验包内容：含 `src/`、`tests/`、`README.md`、`LICENSE`、`CHANGELOG.md`、`Cargo.toml`（feature 声明 + optional 依赖完整），不含 `target/`、`.git/`、`docs/`、`.opencode/`。
4. `cargo package` 后本地解包，人工确认 `Cargo.toml` 内 `[features]` 段含 `tui`/`config`/`schema`/`acp-sse`/`providers-http`。

**阶段 B — 发布（需 crates.io 凭证 + PM 决策，独立于阶段 A）**

5. `cargo publish`（或先 `cargo publish --dry-run` 验证）。
6. 打 annotated tag `v0.2.0` 并 `git push origin v0.2.0`。
7. 验证：`cargo install guigu --version 0.2.0 --features tui` 成功。

### 5. 错误处理

- `cargo publish` 报凭证/已存在版本错误 → 停止，报 PM（凭证或版本号需 PM 决策）。
- feature 矩阵任一门禁红 → 修复后重跑，不得带红发布。
- `cargo package --list` 出现多余文件 → 修正 `include` 白名单后重跑（参照 019 的 `include` 锚定教训）。

### 6. 边界声明

- **不改 `src/` `tests/` 逻辑**；仅版本号 + 根文档（CHANGELOG/README）。
- **不引入新依赖**（feature 与 optional 依赖均已定稿）。
- `docs/` 内部任务文档本任务不动（架构文档如需同步 feature 措辞，属后续维护任务，不在本任务范围）。
- 实际 `cargo publish` 需要 crates.io 凭证 + PM 授权，作为阶段 B 独立决策，不阻塞阶段 A 准备。

## Files

- Cargo.toml（仅 `version` 字段）
- CHANGELOG.md（新增 `[0.2.0]` 段）
- README.md（版本号 + feature flags 表 + Features 小节）

## Acceptance Criteria

- [x] `Cargo.toml` `version = "0.2.0"`；`[features]` 段含 `providers-http`/`config`/`schema`/`tui`/`acp-sse` 五者，`default = ["providers-http", "config", "schema"]`
- [x] CHANGELOG.md 新增 `[0.2.0]` 段，覆盖 022–029 能力 + 030–038 维护概括
- [x] README.md 三处版本号改为 `0.2.0`，feature flags 表完整 5 行
- [x] `cargo check --all-features` passes
- [x] `cargo check --no-default-features` passes
- [x] `cargo test --all-targets` passes
- [x] `cargo test --all-features` passes
- [x] `cargo clippy --all-targets --all-features -D warnings` passes
- [x] `cargo fmt --check` passes
- [x] `cargo package --list` 含必要文件、不含 `target/` `.git/` `docs/` `.opencode/`
- [x] （阶段 B，PM 决策）`cargo publish` 成功；`cargo install guigu --version 0.2.0 --features tui` 成功；tag `v0.2.0` 存在

## 修订记录

- v1.0（2026-09-13，Architect）：初稿。定位 `cargo install --features tui` 失败根因 = crates.io v0.1.0 为六期快照、缺九期才加入的 `tui` feature；发布 v0.2.0 minor bump 补齐九~十一期能力。分「准备（阶段 A）」与「发布（阶段 B，需凭证+PM 决策）」两阶段。
- v1.1（2026-09-13，Architect）：验收清单补记。v0.2.0 已实际发布完成——`Cargo.toml` version=0.2.0、CHANGELOG `[0.2.0] - 2026-09-13`、README 版本号与 5 行 feature flags 表均已就位；阶段 A 经 reviewer approve（commit `d1d4270`），阶段 B `cargo publish` + tag `v0.2.0` 完成（commit `1c4d47a`，tag 存在）。勾选全部验收项。
