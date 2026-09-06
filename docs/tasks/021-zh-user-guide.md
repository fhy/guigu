# Task 021: 中文用户文档 + Provider 配置说明

## Background

v0.1.0 已发布（002–020 全部闭环），`README.md`（根，英文）提供了简介/安装/快速开始/feature flags/架构链接，但面向中文用户的使用文档缺失。此外，Provider 的接入方式存在**两层能力边界**，尚未在用户文档中明确：

1. **内置 adapter 层**：库只内置 OpenAI / Anthropic 两个 adapter（007 交付），但二者 `base_url` 均可配置——意味着任何「OpenAI 兼容」或「Anthropic 兼容」端点都能直接接入（Ollama、vLLM、DeepSeek 等），而非只能连官方服务。
2. **trait 层（嵌入库开放能力）**：`ModelProvider` trait（`core/provider.rs`，003 定稿）是公开扩展点，嵌入方可自行 `impl ModelProvider` 接入任意后端（协议不兼容的服务、私有模型、自定义鉴权/重试），**无需修改库代码**。

PM 指令：① 增加中文用户文档；② 把上述两层 Provider 接入能力（内置只有 OpenAI/Anthropic + base_url 可配置；trait 层自定义 provider 为开放能力）如何配置，写入文档。

## Goal

新增一份中文用户文档 `docs/user-guide.zh.md`，覆盖：简介、安装与 feature flags、快速开始（库 / CLI）、核心抽象、**Provider 配置（重点，两层能力）**、相关文档。纯文档，零代码变化。

## Design Notes

### 文档定位与边界

- 面向**中文用户**：定位为「用户文档」，与 `README.md`（英文，面向 crates.io 首页）互补；不做架构细节展开（链接 `docs/architecture.md`）。
- 示例统一标注「契约示意」：Architect 不读 `src/`，示例以 `docs/architecture.md` §3、`docs/tasks/003` / `docs/tasks/007` 规格为权威，避免逐字绑定构造器签名（与 019 README 的既有约定一致）。
- 核心价值段落 = **Provider 配置**（PM 第 ② 点），必须写清两层边界与决策表。

### Provider 两层能力（文档核心，须准确）

**第一层：内置 adapter（开箱即用）**
- 仅内置 OpenAI（Chat Completions）与 Anthropic（Messages）两个 adapter，位于 `src/adapters/`，由 `providers-http` feature 门控（默认开启）。
- 二者 `base_url` 均可配置（默认官方端点）：`OpenAiConfig { api_key, base_url }`、`AnthropicConfig { api_key, base_url, max_tokens, anthropic_version }`（见 007 规格）。
- **关键推论（必须写进文档）**：`base_url` 可配置 → 任何「OpenAI 兼容」(`/chat/completions`) 或「Anthropic 兼容」(`/messages`) 端点都能直接接入。「内置只有 OpenAI/Anthropic」指**协议适配器**只有两种，不是只能连两家官方服务。

**第二层：trait 层（嵌入库开放能力）**
- `ModelProvider` trait 定义于 `core/provider.rs`（003 定稿），**始终可用，不受 `providers-http` feature 门控**（即 `default-features = false` 剥离 reqwest 后，trait 仍在，仍可自定义 provider）。
- 嵌入方自行 `impl ModelProvider`（`stream(ProviderRequest) -> Result<AssistantStream, ProviderError>`）接入任意后端，不改库。
- 适用场景：协议不兼容 OpenAI/Anthropic（如 Gemini 原生 API）、私有模型服务、自定义鉴权/重试/缓存。
- 错误两段式契约（003 已定）：请求建立失败 → 外层 `Err(ProviderError)`；流建立后失败 → 流内 `AssistantEvent::Error`。

### 决策表（帮助用户选层）

| 场景 | 选择 |
|------|------|
| OpenAI / Anthropic 官方 | 内置 provider（默认 base_url） |
| OpenAI 兼容第三方（Ollama / vLLM / DeepSeek / Moonshot / OpenRouter…） | 内置 `OpenAiProvider` + 自定义 `base_url` |
| Anthropic 兼容网关 / 代理 | 内置 `AnthropicProvider` + 自定义 `base_url` |
| 协议不兼容 / 私有后端 / 特殊鉴权重试 | 自定义 `impl ModelProvider` |

### 文档结构（建议章节）

1. 简介（一句话定位 + 能力清单）
2. 安装与 feature flags（`providers-http` 默认开启；`default-features = false` 剥离 reqwest；`acp-sse` 预留存根）
3. 快速开始（3.1 库嵌入：AgentHandle 生命周期示意；3.2 CLI：`guigu run` / `guigu acp` + 关键参数表）
4. 核心抽象（Agent / Tool / ModelProvider / Plugin 各一段，trait 签名示意）
5. **Provider 配置（重点）**：5.1 内置 adapter 层（OpenAI/Anthropic + base_url 可配置 + 兼容端点说明）；5.2 trait 层（自定义 provider 开放能力 + 契约 + 错误两段式）；5.3 如何选择（决策表）
6. 相关文档（architecture.md / TASK_BOARD.md / README.md）

### 错误处理

无代码，无错误路径。仅文档措辞需与 003/007/architecture §3.5 一致。

### 边界声明（明确不做）

- 不改 `src/` `tests/` `Cargo.toml`。
- 不改根 `README.md`（根文件归属需 PM 单独授权；如需在 README 加中文文档链接，列为后续可选）。
- 不新增依赖、不动代码。

## Files

- docs/user-guide.zh.md（新增，中文用户文档）
- docs/tasks/021-zh-user-guide.md（本规格）

## Acceptance Criteria

- [ ] `docs/user-guide.zh.md` 存在，全文中文
- [ ] 覆盖：简介 / 安装与 feature flags / 快速开始（库 + CLI）/ 核心抽象 / Provider 配置 / 相关文档
- [ ] Provider 配置章节明确写出两层边界：① 内置仅 OpenAI/Anthropic 且 `base_url` 可配置（含「协议适配器仅两种 ≠ 只能连官方」的推论）；② `ModelProvider` trait 为嵌入库开放能力（`default-features = false` 下仍可自定义）
- [ ] 含 Provider 决策表（官方 / 兼容端点 / 自定义 trait 三选）
- [ ] 示例标注「契约示意」，不逐字绑定未核对的构造器签名，指向 architecture §3.5 / 003 / 007 为权威
- [ ] 与 007 规格一致：`OpenAiConfig`/`AnthropicConfig` 字段、`ModelProvider::stream` 签名、错误两段式
- [ ] 零代码变化（不触碰 src/tests/Cargo.toml）

## 修订记录

- v1.0（2026-09-06，Architect）：初稿。新增中文用户文档 `docs/user-guide.zh.md`，核心为 Provider 两层接入能力说明（内置 OpenAI/Anthropic + base_url 可配置；trait 层自定义 provider 开放能力）+ 决策表。纯文档，零代码。
