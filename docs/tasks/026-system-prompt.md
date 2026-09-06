# Task 026: CLI 自定义 system prompt + 默认身份鬼谷子 + 内联 base_url 端点覆盖

## Background

`guigu` 的 agent 身份由 `AgentConfig.system_prompt` 决定（001 定稿：`AgentConfig { system_prompt, model, thinking_level }`，经 `AgentSnapshot.system_prompt` → `Context` → 007 adapter 进入请求体）。当前 CLI 装配层（`src/bin/guigu/assemble.rs`）把该字段硬编码为常量 `SYSTEM_PROMPT = "You are guigu, a helpful coding assistant."`，用户无法自定义身份。

同时，015 的 CLI 只有 `-p openai|anthropic` + `-m <model>` + `-k <key>`，没有 `--base-url`，用户想接 Ollama/DeepSeek/vLLM/自建网关等「OpenAI/Anthropic 兼容端点」必须改代码重编译（021 已文档化 adapter `base_url` 可配置，但 CLI 未暴露）。

`~/guigu-ms` 仓库已有未提交草稿改动（`src/bin/guigu/cli.rs` + `assemble.rs`）：`cli.rs` 新增全局参数 `--system-prompt` 与 `--base-url`，`assemble.rs` 把 `system_prompt` 透传进 `AgentConfig`、把 `base_url` 透传进 adapter config。本任务把草稿**正式化**：补单元测试、把默认身份改为「鬼谷子」，并通过 DoD 门禁与合规提交。

> ⚠ 草稿位于 `~/guigu-ms`（与规范仓库 `/home/fhy/guigu/` 是两份拷贝）。Developer 落地时须把改动落到 PM 确认的目标仓库（见 Acceptance Criteria 备注）。

## Goal

- 正式化 `--system-prompt <TEXT>` 全局参数（`run` 与 `acp` 两模式均生效）。
- 把默认身份常量改为鬼谷子，不传 `--system-prompt` 时也使用鬼谷子身份。
- 正式化 `--base-url <URL>` 全局参数：内联端点覆盖，透传到 adapter config（`run` 与 `acp` 两模式均生效）。
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

- 在**入口一处**调用一次，得到具体 `String`，再作为参数传给 `assemble`/`build_server`。
- `assemble`/`build_server` 签名收**已解析的 `system_prompt: String`**（不重复做回退），注入 `AgentConfig { system_prompt, .. }`。这使回退逻辑可独立单测，也避免各函数各自解析。
- 草稿当前是 `assemble` 内联 `cli.system_prompt.clone().unwrap_or_else(|| SYSTEM_PROMPT.to_string())`——**需按本规格提炼为 `resolve_system_prompt` + 改名常量**，非原样照搬草稿。

### 3. CLI 参数（src/bin/guigu/cli.rs）

```rust
#[derive(Parser)]
pub struct Cli {
    /// 自定义 system prompt（缺省使用鬼谷子默认身份）。
    #[arg(long, global = true, value_name = "TEXT")]
    pub system_prompt: Option<String>,

    /// 覆盖 provider 的 base URL（如 ModelScope/本地网关）。
    #[arg(long, global = true, value_name = "URL")]
    pub base_url: Option<String>,
    // ... 既有字段（-m/-p/-s/-c/-l/-k 等）与 subcommand（run/acp）不变
}
```

- 两参数均 `global = true`：`guigu --system-prompt "..." acp` 与 `guigu acp --system-prompt "..."` 均可解析；`run`/`acp` 两模式共用。
- 字段类型均为 `Option<String>`：未传为 `None`（`system_prompt` 触发回退；`base_url` 走协议默认端点）。

### 4. base_url 内联端点覆盖（assemble.rs build_provider）

```rust
let base_url = cli.base_url.clone();
match cli.provider {
    Provider::Openai => {
        let mut config = OpenAiConfig::new(key);
        config.base_url = base_url;
        Ok(Arc::new(OpenAiProvider::new(config)?))
    }
    Provider::Anthropic => {
        let mut config = AnthropicConfig::new(key);
        config.base_url = base_url;
        Ok(Arc::new(AnthropicProvider::new(config)?))
    }
    // Fake 早退（不读 base_url），此处穷尽 match 不 panic。
    Provider::Fake => Ok(Arc::new(FakeProvider)),
}
```

- `--base-url` 仅做「内联端点覆盖」：`None` → adapter 用协议默认端点（007 既有语义，不改）；`Some(url)` → 写入 `OpenAiConfig.base_url` / `AnthropicConfig.base_url`。
- 仅复用 007 既有 `base_url` 字段，**不新增 HTTP 逻辑、不改 adapter 构造签名**。
- `base_url` 透传为 `Option<String>` 直接赋值，无失败路径；Fake provider 早退不受影响。

### 5. 透传链

```
# system_prompt 链
cli.system_prompt (Option<String>)
  → resolve_system_prompt(...)           // 入口一处解析
  → assemble(..., system_prompt: String) // assemble.rs
  → build_server(..., system_prompt)     // assemble.rs
  → AgentConfig { system_prompt, .. }    // 001 定稿字段
  → AgentSnapshot.system_prompt          // 可离线断言
  → Context → 007 adapter 请求体          // 真 LLM 生效

# base_url 链
cli.base_url (Option<String>)
  → build_provider(cli) 内               // assemble.rs
  → OpenAiConfig/AnthropicConfig { base_url, .. }
  → OpenAiProvider/AnthropicProvider 请求端点
```

### 6. 错误处理

- 本任务无新增错误类型：`--system-prompt`/`--base-url` 解析由 clap 负责（非法/缺失值由 clap 报错退出）。
- `resolve_system_prompt` 为纯函数，无失败路径；产品代码无 `unwrap()`（`unwrap_or_else` 非 `unwrap`）。

## Files

- `src/bin/guigu/cli.rs`（新增 `--system-prompt` + `--base-url` 全局参数 + `#[cfg(test)]` 参数解析单测）
- `src/bin/guigu/assemble.rs`（`DEFAULT_SYSTEM_PROMPT` 常量改鬼谷子 + `resolve_system_prompt` + `build_provider` 透传 `base_url` + `assemble`/`build_server` 透传 `system_prompt` + 单测）
- `src/bin/guigu/main.rs`（若 `assemble` 签名改为收已解析 String，入口处调一次 `resolve_system_prompt`；若草稿已在 assemble 内解析则确认路径，仅保证回退只发生在一处）
- 可选 `tests/cli.rs`（若已存在则补 `--system-prompt`/`--base-url` 冒烟；否则仅用 bin 内 `#[cfg(test)]` 覆盖）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets -D warnings passes
- [ ] cargo test --all-targets passes
- [ ] cargo fmt --check passes
- [ ] 参数解析单测：`try_parse_from(["guigu", "--system-prompt", "你是鬼谷子", "acp"])` → `system_prompt == Some("你是鬼谷子")`；不带 flag → `None`；`--system-prompt` 位于 subcommand 后（`guigu acp --system-prompt ...`）同样生效（`global = true`）
- [ ] 参数解析单测（base_url）：`try_parse_from(["guigu", "--base-url", "http://localhost:11434/v1", "acp"])` → `base_url == Some(...)`；不带 flag → `None`
- [ ] 缺省回退单测：`resolve_system_prompt(None) == DEFAULT_SYSTEM_PROMPT`；`resolve_system_prompt(Some("自定义")) == "自定义"`
- [ ] 离线验证（无需网络）：装配后 `AgentConfig.system_prompt`（或经 `AgentSnapshot.system_prompt`）等于传入的自定义文案 / 默认鬼谷子身份
- [ ] 手动/可选（需真 adapter 网络）：`guigu acp --system-prompt "你是鬼谷子..."` 首条回复体现自定义身份；不传时回复体现默认鬼谷子身份；`--base-url` 指向本地 Ollama/网关时请求打到自定义端点
- [ ] 产品代码无 `unwrap()`；公开项（`DEFAULT_SYSTEM_PROMPT`、`resolve_system_prompt`、`Cli.system_prompt`、`Cli.base_url`）均有 `///` 文档注释
- [ ] 单文件 ≤ 400 行

### 边界（明确不做）

- **不改 ACP 协议**：不新增/改 JSON-RPC 方法、不触碰 014/025 的 wire 格式；`--system-prompt`/`--base-url` 仅改变 agent 装配输入。
- **不做配置文件加载**：`--system-prompt`/`--base-url` 仅来自 CLI 参数，不读 toml/环境变量/默认文件（配置文件加载属 022）。
- **base_url 分工**：本任务只做「内联 `--base-url` 简单透传」；完整配置化（TOML 配置文件 + `ProviderFactory` + `-m` 语义扩展 + api_key 四段链 + `--config`/`--api-key-env`）仍属 022，022 的「新增 --base-url」已由本任务落地、改为复用。
- **不改 `AgentConfig` 结构**：`system_prompt` 字段 001 已定，本任务只赋不同值。

## 修订记录

- v1.0（2026-09-06，Architect）：初稿。正式化 `~/guigu-ms` 草稿（`--system-prompt` 全局参数 + assemble 透传）；默认身份常量改鬼谷子；新增 `resolve_system_prompt` 纯函数统一缺省回退；单测覆盖参数解析 + 缺省回退；离线 snapshot 断言 + 可选真 adapter 验证；边界明确不改 ACP / 不做配置文件加载 / 不碰 base_url。
- v1.1（2026-09-06，Architect，依据 PM「迁回这 2 个 src 改动」授权）：草稿 `cli.rs` 实际同时含 `--base-url` 全局参数、`assemble.rs` 实际含 `build_provider` 的 `base_url` 透传（原 v1.0 边界误记为「不涉及 base_url」，与草稿内容冲突）。本次将 `--base-url` 内联端点覆盖纳入 026 范围（仅简单透传复用 007 既有 `base_url` 字段），与 022 明确分工：026 只做内联透传、022 做完整配置化并复用 026 已落地的 `--base-url`。
