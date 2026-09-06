# Task 022: 自定义模型配置化接入（Provider 配置 + CLI 参数扩展）

## Background

021 已文档化 Provider 的「两层接入能力」：内置 adapter（OpenAI/Anthropic，`base_url` 可配置）覆盖任意 OpenAI/Anthropic 兼容端点；`ModelProvider` trait 是嵌入库开放扩展点。但**两层都存在真实缺口**：

1. **CLI 层无法配置自定义模型**：015 的 CLI 只有 `-p openai|anthropic` + `-m <model>` + `-k <key>`，没有 `--base-url`，也没有配置文件。用户想接 Ollama/DeepSeek/vLLM/自建网关，必须改代码重编译——违背「可独立运行」目标。
2. **库层没有「配置 → provider」的标准工厂**：每个嵌入方要自己手写 `match protocol { ... }` 装配逻辑，重复且易错。

本任务补齐「自定义模型」的**配置化接入**：一个 serde 可反序列化的 `ModelConfig` 描述模型端点，一个工厂从配置构建 `Arc<dyn ModelProvider>`（复用 007 两个 adapter），CLI 通过配置文件 + 新参数开箱即用任意兼容端点。

## Goal

- 新增 `ModelConfig` / `Protocol` / `ProviderConfigError`：serde 反序列化的模型端点描述 + 校验。
- 新增配置文件加载：TOML 多模型配置（`guigu.toml` / `~/.config/guigu/config.toml`）。
- 新增 `ProviderFactory`：从 `ModelConfig` 构建 `Arc<dyn ModelProvider>`（复用 007 `OpenAiProvider`/`AnthropicProvider`）。
- CLI 扩展：`--config`、`--base-url`、`--api-key-env`；`-m` 语义扩展为「配置名优先，否则内联 model id」。

## Design Notes

### 1. 数据结构（src/config.rs，不 feature-gate，纯 serde）

```rust
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol { OpenAi, Anthropic }

#[derive(Debug, Clone, Deserialize)]
pub struct ModelConfig {
    pub name: String,                   // 配置键名（唯一标识），加载时注入
    pub protocol: Protocol,
    #[serde(default)] pub base_url: Option<String>,     // None → 协议默认端点
    #[serde(default)] pub api_key: Option<String>,      // 明文 key（低优先级）
    #[serde(default)] pub api_key_env: Option<String>,  // 环境变量名（中优先级）
    pub model: String,                  // 默认 model id（透传给 provider 请求体）
    #[serde(default)] pub max_tokens: Option<u32>,          // Anthropic 用，缺省 4096
    #[serde(default)] pub anthropic_version: Option<String>, // Anthropic 用，缺省 "2023-06-01"
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderConfigError {
    #[error("unknown protocol: {0}")] UnknownProtocol(String),
    #[error("missing api_key (neither api_key nor api_key_env set)")] MissingApiKey,
    #[error("api_key_env `{0}` is not set")] ApiKeyEnvUnset(String),
    #[error("config parse error: {0}")] Parse(String),      // 包装 toml/serde 错误
    #[error("build error: {0}")] Build(String),              // 包装 ProviderError
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct GuiguConfig {                    // 顶层配置文件
    #[serde(default)] pub models: HashMap<String, ModelConfig>,
}
```

- `Config::load(path: &Path) -> Result<GuiguConfig, ProviderConfigError>`（`#[cfg(feature="config")]`）：读文件 + `toml::from_str`，把每个 `models` 表键注入为对应 `ModelConfig.name`。
- `Config::resolve(path: Option<&Path>) -> Result<GuiguConfig, ProviderConfigError>`（`#[cfg(feature="config")]`）：查找顺序 `--config` 显式 → `./guigu.toml` → `$XDG_CONFIG_HOME/guigu/config.toml`（缺省 `~/.config/guigu/config.toml`）→ 都不存在返回 `Default`（空配置，向后兼容）。不引入 `directories` crate，手动 `std::env`。

### 2. api_key 解析链（ModelConfig::resolve_api_key）

优先级从高到低：

1. CLI `-k/--api-key`（显式传入，最高）
2. `ModelConfig.api_key`（配置文件明文）
3. `ModelConfig.api_key_env` 指向的环境变量
4. 协议默认 env：`OpenAi → OPENAI_API_KEY`，`Anthropic → ANTHROPIC_API_KEY`（对齐 015 语义）

最终为空 → 返回 `ProviderConfigError::MissingApiKey`（本地端点如需占位 key，用户在配置里显式 `api_key = "ollama"` 即可，不做隐式占位，避免掩盖配置错误）。

### 3. 工厂（src/adapters/factory.rs，`#[cfg(feature = "providers-http")]`）

```rust
pub fn build_provider(config: &ModelConfig, api_key: &str)
    -> Result<Arc<dyn ModelProvider>, ProviderConfigError>;
```

- `Protocol::OpenAi` → `OpenAiConfig { api_key, base_url }` → `OpenAiProvider::new(...)`（007 定稿签名）。
- `Protocol::Anthropic` → `AnthropicConfig { api_key, base_url, max_tokens: 默认 4096, anthropic_version: 默认 "2023-06-01" }` → `AnthropicProvider::new(...)`。
- 工厂仅做「配置 → 007 既有 adapter 构造」的薄封装，**不新增任何 HTTP 逻辑**；007 的 `build_request`/`map_event`/`SSE` 全部复用。
- 工厂 gate 在 `providers-http`（依赖 007 adapter）；`ModelConfig`/`Protocol`/`GuiguConfig` **数据结构（serde）不 gate**（`default-features=false` 下仍可反序列化配置、自行实现 provider）；TOML 文件解析（`Config::load`/`resolve`）**gate 在 `config`**（依赖 `toml`）。

### 4. CLI 参数扩展（src/bin/guigu/cli.rs + assemble.rs）

在 015 + 026 既有参数上追加：

| 参数 | 说明 |
|------|------|
| `--config <FILE>` | 配置文件路径（缺省走 `resolve` 查找链） |
| `--base-url <URL>` | 内联端点覆盖（**已由 026 落地**，本任务复用，不重复新增） |
| `--api-key-env <VAR>` | 内联指定 key 来源 env（配合内联 `-m` 场景，可选） |

`-m/--model` 语义扩展（向后兼容）：

1. 若 `models` 中存在 `name == <model>` → 用该 `ModelConfig`（protocol/base_url/key 全部来自配置）。
2. 否则视为内联 model id，用 `-p` 协议 + `--base-url`（若有）+ `-k`/env key（015 原语义）。

装配逻辑（assemble.rs）改动：把「选 provider」从「`-p` 二分」改为「先查配置、再回退内联」，其余（tools/AgentServer/session）不变。

### 5. 依赖与 feature

```toml
[features]
default = ["providers-http", "config"]   # config 加入 default（理由见下）
config = ["dep:toml"]

[dependencies]
toml = { version = "0.8", optional = true }   # feature-gated（PM 签核）
# ratatui/crossterm 不在此任务（023 TUI）
```

- `toml` 为 **optional 依赖**，由 `config` feature 门控（PM 签核：新依赖均 feature-gated 引入）。`config` **default 开启**：`toml` 极轻（纯 serde 解析、无 TLS/系统依赖），CLI 自定义模型配置应开箱即用（`cargo install guigu` 默认可用），且 default `cargo test` 需覆盖配置解析（对标 007 `providers-http` default 的测试覆盖理由）。
- feature 拆分边界：`ModelConfig`/`Protocol`/`GuiguConfig` **数据结构不 gate**（纯 serde）；`Config::load`/`Config::resolve`（TOML 解析）**gate 在 `config`**；`ProviderFactory` **gate 在 `providers-http`**（复用 007 adapter）。
- 嵌入方极致 minimal 用 `default-features = false` 剥离 `toml` + `reqwest`（既有先例，021 已文档化）。
- **不引入**：`figment`/`config`/`serde_yaml`（重/多余）、`directories`（手动 env）、`schemars`（roadmap 候选 3，独立）。

### 6. 边界声明（明确不做）

- **新协议 adapter**（Gemini/Bedrock 原生协议）：不在本任务；自定义后端仍走「OpenAI/Anthropic 兼容端点」或「`impl ModelProvider`」两层（021 已定）。
- **全局 config 合并**（把 `--session`/`--cwd`/`--log` 也纳入配置文件）：不在本任务，仅做模型配置。
- **运行时热切换 provider** / 多模型路由（按 message 选模型）：不在本任务，单会话单 provider。
- **api_key 加密存储**：不在本任务；敏感 key 用 `api_key_env` 引用环境变量（业界惯例），配置文件不强制加密。

## Files

- src/config.rs（`ModelConfig`/`Protocol`/`GuiguConfig`/`ProviderConfigError` 不 gate + `load`/`resolve` `#[cfg(feature="config")]` + api_key 解析 + 单元测试）
- src/adapters/factory.rs（`build_provider`，`#[cfg(feature="providers-http")]` + 单元测试）
- src/lib.rs（re-export `config` 模块；factory 在 `#[cfg(feature="providers-http")]` 下 re-export）
- src/bin/guigu/cli.rs（新增 `--config`/`--api-key-env` 参数；`--base-url` 已由 026 落地，本任务复用）
- src/bin/guigu/assemble.rs（选 provider 逻辑改为「配置优先、内联回退」）
- Cargo.toml（新增 `toml`）
- tests/config.rs（配置解析/工厂构建集成测试；工厂端到端可用 wiremock 验证 `base_url` 生效，复用 007 测试模式）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets -D warnings passes
- [ ] cargo test --all-targets passes
- [ ] cargo fmt --check passes
- [ ] `ModelConfig` TOML 反序列化：protocol（openai/anthropic）、可选 base_url/api_key/api_key_env/max_tokens/anthropic_version、name 由表键注入均正确
- [ ] `resolve_api_key` 四段优先级（CLI > 明文 > api_key_env > 协议默认 env）逐条正确；最终为空 → `MissingApiKey`
- [ ] `Config::resolve` 查找链（显式 → ./guigu.toml → ~/.config/guigu/config.toml → 空）正确；文件缺失不报错返回空配置
- [ ] `build_provider`：OpenAi/Anthropic 两条分支构造正确；wiremock 起本地 mock，断言 `base_url` 覆盖后请求打到自定义地址（复用 007 测试模式，不依赖外网）
- [ ] CLI：`-m <配置名>` 命中配置（protocol/base_url/key 来自配置）；`-m <内联id>` + `--base-url` 走内联路径；二者向后兼容
- [ ] 产品代码无 `unwrap()`；配置错误映射为 `ProviderConfigError` + 非零退出/stderr 提示
- [ ] `cargo test --no-default-features`：核心库编译通过、`config`/`toml` 被剥离（`Config::load`/`resolve` 不存在），`ModelConfig`/`Protocol`/`GuiguConfig` 数据结构仍可用
- [ ] 单文件 ≤ 400 行，超则拆子模块并记录
- [ ] 新增依赖仅 `toml`（optional，`config` feature），无其它

## 修订记录

- v1.0（2026-09-06，Architect）：初稿。`ModelConfig`/`Protocol`/`GuiguConfig` serde 反序列化 + `ProviderFactory`（复用 007 adapter）+ CLI `--config`/`--base-url`/`--api-key-env` + `-m` 语义扩展（配置名优先）；`toml` 为普通依赖（配置解析与 HTTP 无关）；api_key 四段解析链；边界排除新协议 adapter / 全局 config 合并 / 多模型路由 / key 加密。
- v1.1（2026-09-06，Architect，依据 PM 签核）：① 自定义模型边界确认**方案 B**（`--base-url` + TOML profile + `api_key` 可选），本规格 scope 即方案 B，无新增/缩减；② `toml` 由「普通依赖」改为 **optional + `config` feature（default 开启）**——feature 边界：数据结构（serde）不 gate、`Config::load`/`resolve`（TOML 解析）gate 在 `config`、`ProviderFactory` gate 在 `providers-http`；新增 `--no-default-features` 剥离验证。
- v1.2（2026-09-06，Architect，协调 026）：026 草稿已含 `--base-url` 全局参数 + `build_provider` 内联透传，现纳入 026 范围（内联端点覆盖）。本规格「新增 `--base-url`」改为「复用 026 已落地」；本任务仍负责完整配置化（`--config`/`--api-key-env` + TOML 配置 + `ProviderFactory` + `-m` 语义扩展 + api_key 四段链），scope 不减。
