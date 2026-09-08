//! Provider 工厂（Task 022）：从 [`ModelConfig`] 构建 `Arc<dyn ModelProvider>`。
//!
//! 整个 `adapters` 模块 gate 在 `providers-http`（依赖 007 adapter）。工厂仅做
//! 「配置 → 007 既有 adapter 构造」的薄封装，**不新增任何 HTTP 逻辑**；007 的
//! `build_request`/`map_event`/`SSE` 全部复用。

use std::sync::Arc;

use crate::adapters::anthropic::{
    AnthropicConfig, AnthropicProvider, DEFAULT_ANTHROPIC_VERSION, DEFAULT_MAX_TOKENS,
};
use crate::adapters::openai::{OpenAiConfig, OpenAiProvider};
use crate::config::{ModelConfig, Protocol, ProviderConfigError};
use crate::core::provider::ModelProvider;

/// 从模型配置构建 provider。
///
/// `api_key` 为已解析的 API key（经 [`ModelConfig::resolve_api_key`]）。
/// `Protocol::OpenAi` → [`OpenAiProvider`]；`Protocol::Anthropic` →
/// [`AnthropicProvider`]（`max_tokens`/`anthropic_version` 缺省用 007 默认值）。
pub fn build_provider(
    config: &ModelConfig,
    api_key: &str,
) -> Result<Arc<dyn ModelProvider>, ProviderConfigError> {
    match config.protocol {
        Protocol::OpenAi => {
            let provider = OpenAiProvider::new(OpenAiConfig {
                api_key: api_key.to_string(),
                base_url: config.base_url.clone(),
            })
            .map_err(|e| ProviderConfigError::Build(e.to_string()))?;
            Ok(Arc::new(provider))
        }
        Protocol::Anthropic => {
            let provider = AnthropicProvider::new(AnthropicConfig {
                api_key: api_key.to_string(),
                base_url: config.base_url.clone(),
                max_tokens: config.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
                anthropic_version: config
                    .anthropic_version
                    .clone()
                    .unwrap_or_else(|| DEFAULT_ANTHROPIC_VERSION.to_string()),
            })
            .map_err(|e| ProviderConfigError::Build(e.to_string()))?;
            Ok(Arc::new(provider))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ModelConfig;

    fn openai_config(base_url: Option<&str>) -> ModelConfig {
        ModelConfig {
            name: "test".into(),
            protocol: Protocol::OpenAi,
            base_url: base_url.map(str::to_string),
            api_key: None,
            api_key_env: None,
            model: "gpt-4o".into(),
            max_tokens: None,
            anthropic_version: None,
        }
    }

    fn anthropic_config(base_url: Option<&str>) -> ModelConfig {
        ModelConfig {
            name: "test".into(),
            protocol: Protocol::Anthropic,
            base_url: base_url.map(str::to_string),
            api_key: None,
            api_key_env: None,
            model: "claude-3".into(),
            max_tokens: None,
            anthropic_version: None,
        }
    }

    #[test]
    fn build_openai_provider() {
        let provider = build_provider(&openai_config(None), "sk-test").unwrap();
        // 能构造即通过（Arc<dyn ModelProvider> 非空）。
        assert!(Arc::strong_count(&provider) >= 1);
    }

    #[test]
    fn build_anthropic_provider() {
        let provider = build_provider(&anthropic_config(None), "sk-test").unwrap();
        assert!(Arc::strong_count(&provider) >= 1);
    }

    #[test]
    fn build_provider_with_base_url() {
        let provider =
            build_provider(&openai_config(Some("http://localhost:11434/v1")), "sk-test").unwrap();
        assert!(Arc::strong_count(&provider) >= 1);
    }
}
