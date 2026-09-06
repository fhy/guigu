# Task 019: 发布 v0.1.0（首个版本）

## Background

002–018 已全部实现、审查通过、四门禁全绿（018 补跑 `cargo test --all-targets` 398 passed）。项目已具备发布条件：完整 AI Agent 运行时（核心 + 工具 + 适配器 + session 树 + 远程协议 + server + ACP + CLI + 插件）。

当前发布资产缺失：
- `README.md` 仅一行占位（`# guigu`），无介绍、无用法、无 feature 说明。
- 无 `CHANGELOG.md`、无 `LICENSE` 实体文件（Cargo.toml 已声明 `license = "MIT"`，但缺文件与之对应）。
- 无 git tag。
- `Cargo.toml` 已有 `version = "0.1.0"` / `description` / `license` / `repository`，缺 `readme` / `keywords` / `categories` 等元数据。

## Goal

发布可复现、可打包的 v0.1.0：补齐发布必需文件与元数据，打 annotated tag，本地验证包完整性。**本任务不实际推送 crates.io**（需凭证 + PM 决策，作为后续独立一步）。

## Design Notes

### 交付物

1. **README.md**（完整化）：
   - 一句话简介 + 特性列表（对应已交付能力：trait 抽象 Agent/Tool/Runtime、async-first tokio、session 树 + JSONL 崩溃恢复、多 lane、远程协议、ACP、插件、CLI）。
   - 安装与快速开始：库用法（最小 Echo/自定义 agent 示例）+ CLI 用法（`guigu run` / `guigu acp`，见 015 定稿命令面）。
   - 架构概览（链接 `docs/architecture.md`，不重复展开）。
   - feature flags 表：`default = ["providers-http"]`（含 reqwest 真 LLM 适配器）；`default-features = false` 剥离 reqwest 得纯核心库；`acp-sse` 为预留（当前存根，见 014）。
   - License 声明（MIT）。

2. **CHANGELOG.md**（v0.1.0 变更清单，按能力分组，覆盖 002–018）：
   - 核心运行时（002 Message/Event、001 Agent trait + AgentHandle、003 Tool trait + Runtime、004 Echo Agent）。
   - 内置工具（005 文件 read/write/edit、006 bash + file_mutation_queue）。
   - 适配器（007 OpenAI/Anthropic，reqwest feature-gated）。
   - 上下文（008 Compactor 摘要压缩）。
   - 会话（009 Session 树 + JSONL 崩溃恢复；012 多 lane；017-a/b 加固 + 恢复语义）。
   - 远程与协议（010 NDJSON 双向流；013 Agent Server；014 ACP v1 stdio；015 CLI）。
   - 扩展（011 DeferredTool、016 插件机制）。
   - 技术债收尾（017-c 锁纪律、018 lane 拆分）。

3. **LICENSE**：MIT 全文（与 Cargo.toml `license = "MIT"` 一致）。

4. **Cargo.toml 元数据**：补 `readme = "README.md"`、`keywords = [...]`、`categories = [...]`；`version` 保持 `0.1.0` 不变。

5. **git tag**：`v0.1.0`（annotated，含一句话版本说明），指向发布 commit。

### 发布步骤（Developer 执行）

1. 补 README / CHANGELOG / LICENSE + Cargo.toml 元数据，commit（`docs:` 类型，见下边界说明）。
2. 跑四门禁 + `cargo package --list`（确认包内容含 README/LICENSE/src/tests/Cargo.toml，不含 `target/`、`.git/`）。
3. 打 annotated tag `v0.1.0` 并 push（`git push origin v0.1.0`）。
4. （可选，PM 拍板）`cargo publish --dry-run` 或实际 publish —— 需 crates.io 凭证，独立于本任务 DoD。

### 错误处理

无新增运行时代码；README/CHANGELOG/LICENSE 为纯文本。若 `cargo package` 报告缺文件/多余文件，按提示修正元数据后重跑。

### 边界声明（明确不做）

- **不实际 publish 到 crates.io**（凭证 + PM 决策，后续一步）。
- **不改 `src/` `tests/` 逻辑**；仅 Cargo.toml 元数据段、根文件、git tag。
- **不引入新依赖**。
- **不动 docs/**（架构文档同步属 020）。

## 文件归属（需 PM 确认，跨角色边界）

本任务触及根文件（README/CHANGELOG/LICENSE）与 Cargo.toml，均不在 conventions 角色表的三方目录（docs/ / src+tests/ / docs/reviews/）内。按「Override exception」规则，需 PM 明确授权归属。建议：README/CHANGELOG/LICENSE 视为发布文档由 Architect 撰写（`docs:` 提交），Cargo.toml 元数据由 Developer 提交；或 PM 指定单一 owner。本规格默认按「Architect 写根文档 + Developer 改 Cargo.toml + PM 打 tag/发布」拆分。

## Files

- README.md（根，完整化）
- CHANGELOG.md（根，新增）
- LICENSE（根，新增，MIT 全文）
- Cargo.toml（仅 `[package]` 元数据段补 readme/keywords/categories）

## Acceptance Criteria

- [ ] README.md 完整（简介 / 特性 / 快速开始 lib+CLI / 架构链接 / feature flags 表 / License）
- [ ] CHANGELOG.md 覆盖 002–018 全部能力（分组）
- [ ] LICENSE 为 MIT 全文，与 Cargo.toml `license = "MIT"` 一致
- [ ] Cargo.toml 补 `readme` / `keywords` / `categories`；`version = "0.1.0"` 不变
- [ ] `cargo check` passes
- [ ] `cargo clippy --all-targets -D warnings` passes
- [ ] `cargo test --all-targets` passes（398+，总数不减）
- [ ] `cargo fmt --check` passes
- [ ] `cargo package --list` 内容完整（含 README/LICENSE/src/tests/Cargo.toml，不含 target/.git）
- [ ] git tag `v0.1.0`（annotated）存在并指向发布 commit
- [ ] 不引入新依赖；`src/` `tests/` 无逻辑改动

## 修订记录

- v1.0（2026-09-06，Architect）：初稿。发布 v0.1.0 = 补齐 README/CHANGELOG/LICENSE + Cargo.toml 元数据 + annotated tag + `cargo package` 本地验证；不实际 publish（凭证 + PM 决策后续）。标记根文件/Cargo.toml 跨角色归属，需 PM 授权。
