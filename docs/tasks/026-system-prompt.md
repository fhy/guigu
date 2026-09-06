# Task 026: CLI 自定义 system prompt + 默认身份鬼谷子

## Background

`guigu` 的 agent 身份由 `AgentConfig.system_prompt` 决定（001 定稿：`AgentConfig { system_prompt, model, thinking_level }`，经 `AgentSnapshot.system_prompt` → `Context` → 007 adapter 进入请求体）。当前 CLI 装配层（`src/bin/guigu/assemble.rs`）把该字段硬编码为常量 `SYSTEM_PROMPT = "You are guigu, a helpful coding assistant."`，用户无法自定义身份。

`~/guigu-ms` 仓库已有未提交草稿改动（`src/bin/guigu/cli.rs` + `assemble.rs`）：`cli.rs` 新增全局参数 `--system-prompt`，`assemble.rs` 的 `assemble`/`build_server` 透传 `system_prompt` 到 `AgentConfig`。本任务把草稿**正式化**：补单元测试、把默认身份改为「鬼谷子」，并通过 DoD 门禁与合规提交。

> ⚠ 草稿位于 `~/guigu-ms`（与规范仓库 `/home/fhy/guigu/` 是两份拷贝）。Developer 落地时须把改动落到 PM 确认的目标仓库（见 Acceptance Criteria 备注）。

## Goal

- 正式化 `--system-prompt <TEXT>` 全局参数（`run` 与 `acp` 两模式均生效）。
- 把默认身份常量改为鬼谷子，不传 `--system-prompt` 时也使用鬼谷子身份。
- 补单元测试：参数解析 + 缺省回退。
- 补文档注释，跑四门禁，合规提交。

## Design Notes

### 1. 默认身份常量（assemble.rs）

将硬编码的 `SYSTEM_PROMPT` 改名为 `DEFAULT_SYSTEM_PROMPT`，值改为鬼谷子身份。示例：

```rust
/// 缺省 system prompt：鬼谷子（Guiguzi）AI 编程助手身份。
pub const DEFAULT_SYSTEM_PROMPT: &str = "你是鬼谷子（Guiguzi），鬼谷子 AI 编程助手。\
你精通 Rust、分布式系统与高并发架构，以简洁、严谨、直接的方式协助用户分析问题、\
设计架构、编写与审查代码。";
```

- 身份必须是「鬼谷子（Guiguzi）AI 编程助手」；具体措辞 Developer 可微调，但不得退化为英文泛用助手。
- 常量 `pub`，供 CLI 层与测试引用。

### 2. 缺省回退逻辑（assemble.rs）

```rust
/// 解析最终 system prompt：优先用自定义文案，缺省回退到 DEFAULT_SYSTEM_PROMPT。
pub fn resolve_system_prompt(custom: Option<String>) -> String {
    custom.unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.to_string())
}
```

- 在**入口一处**（`main.rs` 装配前）调用一次，得到具体 `String`，再作为参数传给 `assemble`/`build_server`。
- `assemble`/`build_server` 签名收**已解析的 `system_prompt: String`**（不重复做回退），注入 `AgentConfig { system_prompt, .. }`。这使回退逻辑可独立单测，也避免各函数各自解析。

### 3. CLI 参数（src/bin/guigu/cli.rs）

```rust
#[derive(Parser)]
pub struct Cli {
    /// 自定义 system prompt（缺省使用鬼谷子默认身份）。
    #[arg(long, global = true, value_name = "TEXT")]
    pub system_prompt: Option<String>,
    // ... 既有字段（-m/-p/-s/-c/-l/-k 等）与 subcommand（run/acp）不变
}
```

- `global = true`：`guigu --system-prompt "..." acp` 与 `guigu acp --system-prompt "..."` 均可解析；`run`/`acp` 两模式共用。
- 字段类型 `Option<String>`：未传为 `None`（触发回退）。

### 4. 透传链

```
cli.system_prompt (Option<String>)
  → resolve_system_prompt(...)           // main.rs，一处解析
  → assemble(..., system_prompt: String) // assemble.rs
  → build_server(..., system_prompt)     // assemble.rs
  → AgentConfig { system_prompt, .. }    // 001 定稿字段
  → AgentSnapshot.system_prompt          // 可离线断言
  → Context → 007 adapter 请求体          // 真 LLM 生效
```

### 5. 错误处理

- 本任务无新增错误类型：`--system-prompt` 解析由 clap 负责（非法/缺失值由 clap 报错退出）。
- `resolve_system_prompt` 为纯函数，无失败路径；产品代码无 `unwrap()`（`unwrap_or_else` 非 `unwrap`）。

## Files

- `src/bin/guigu/cli.rs`（新增 `--system-prompt` 全局参数 + `#[cfg(test)]` 参数解析单测）
- `src/bin/guigu/assemble.rs`（`DEFAULT_SYSTEM_PROMPT` 常量改鬼谷子 + `resolve_system_prompt` + `assemble`/`build_server` 透传 + 单测）
- `src/bin/guigu/main.rs`（入口一处调用 `resolve_system_prompt` 后传入装配，若草稿已透传则仅确认路径）
- 可选 `tests/cli.rs`（若已存在则补 `--system-prompt` 冒烟；否则仅用 bin 内 `#[cfg(test)]` 覆盖）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets -D warnings passes
- [ ] cargo test --all-targets passes
- [ ] cargo fmt --check passes
- [ ] 参数解析单测：`try_parse_from(["guigu", "--system-prompt", "你是鬼谷子", "acp"])` → `system_prompt == Some("你是鬼谷子")`；不带 flag → `None`；`--system-prompt` 位于 subcommand 后（`guigu acp --system-prompt ...`）同样生效（`global = true`）
- [ ] 缺省回退单测：`resolve_system_prompt(None) == DEFAULT_SYSTEM_PROMPT`；`resolve_system_prompt(Some("自定义")) == "自定义"`
- [ ] 离线验证（无需网络）：装配后 `AgentConfig.system_prompt`（或经 `AgentSnapshot.system_prompt`）等于传入的自定义文案 / 默认鬼谷子身份
- [ ] 手动/可选（需真 adapter 网络）：`guigu acp --system-prompt "你是鬼谷子..."` 首条回复体现自定义身份；不传时回复体现默认鬼谷子身份
- [ ] 产品代码无 `unwrap()`；公开项（`DEFAULT_SYSTEM_PROMPT`、`resolve_system_prompt`、`Cli.system_prompt`）均有 `///` 文档注释
- [ ] 单文件 ≤ 400 行

### 边界（明确不做）

- **不改 ACP 协议**：不新增/改 JSON-RPC 方法、不触碰 014/025 的 wire 格式；`--system-prompt` 仅改变 agent 装配输入。
- **不做配置文件加载**：`--system-prompt` 仅来自 CLI 参数，不读 toml/环境变量/默认文件（配置文件加载属 022）。
- **不涉及 base_url**：base_url 相关工作由 022 覆盖，本任务不碰。
- **不改 `AgentConfig` 结构**：`system_prompt` 字段 001 已定，本任务只赋不同值。

## 修订记录

- v1.0（2026-09-06，Architect）：初稿。正式化 `~/guigu-ms` 草稿（`--system-prompt` 全局参数 + assemble 透传）；默认身份常量改鬼谷子；新增 `resolve_system_prompt` 纯函数统一缺省回退；单测覆盖参数解析 + 缺省回退；离线 snapshot 断言 + 可选真 adapter 验证；边界明确不改 ACP / 不做配置文件加载 / 不碰 base_url。
