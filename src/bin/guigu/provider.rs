//! Provider 选择（Task 022）：配置优先、内联回退。
//!
//! 从 `assemble` 抽出（单文件 ≤ 400 行约束）：选 provider 是独立关注点，
//! 复用 022 `Config`/`ModelConfig` + 007 工厂 `build_provider`。

use std::sync::Arc;

use guigu::adapters::build_provider;
use guigu::config::{Config, ModelConfig, Protocol, ProviderConfigError};
use guigu::core::provider::ModelProvider;

use super::cli::{Cli, Provider};
use super::error::CliError;
use super::fake::FakeProvider;

/// 选 provider 的产物：provider + 最终 model id。
pub struct ProviderSelection {
    /// 构建好的 provider。
    pub provider: Arc<dyn ModelProvider>,
    /// 最终 model id（配置命中 → 配置 `model`；内联 → `-m` 或协议默认）。
    pub model: String,
}

/// 选 provider + model id（Task 022：配置优先、内联回退）。
///
/// 1. `Provider::Fake` 早退（离线冒烟，无 key）。
/// 2. 加载配置（`Config::resolve`）。若 `-m` 命中配置名 → 用该 `ModelConfig`
///    （protocol/base_url/key 全部来自配置），model id 用配置的 `model`。
/// 3. 否则内联回退：`-p` 协议 + `--base-url` + `-k`/`--api-key-env`/env key
///    （015 原语义），model id 用 `-m` 或协议默认。
pub fn select_provider(cli: &Cli) -> Result<ProviderSelection, CliError> {
    // 1. Fake 早退。
    if matches!(cli.provider, Provider::Fake) {
        return Ok(fake_selection(cli));
    }

    // 2. 配置优先：-m 命中配置名。
    let config = Config::resolve(cli.config.as_deref())?;
    if let Some(name) = &cli.model
        && let Some(model_config) = config.models.get(name)
    {
        let key = model_config
            .resolve_api_key(cli.api_key.as_deref())
            .map_err(|e| map_config_error(e, model_config))?;
        let provider = build_provider(model_config, &key)?;
        return Ok(ProviderSelection {
            provider,
            model: model_config.model.clone(),
        });
    }

    // 3. 内联回退：-p 协议 + --base-url + -k/--api-key-env/env key。
    let protocol = match cli.provider {
        Provider::Openai => Protocol::OpenAi,
        Provider::Anthropic => Protocol::Anthropic,
        // 防御分支：上方已对 Fake 早退，此处仅为穷尽 match（不 panic）。
        Provider::Fake => return Ok(fake_selection(cli)),
    };
    let model_id = cli
        .model
        .clone()
        .unwrap_or_else(|| cli.provider.default_model().to_string());
    let model_config = ModelConfig {
        name: "inline".to_string(),
        protocol,
        base_url: cli.base_url.clone(),
        api_key: None,
        api_key_env: cli.api_key_env.clone(),
        model: model_id,
        max_tokens: None,
        anthropic_version: None,
    };
    let env = cli.provider.api_key_env();
    let key = model_config
        .resolve_api_key(cli.api_key.as_deref())
        .map_err(|e| match e {
            // 保留 015 友好提示（含 provider 名 + 协议默认 env）。
            ProviderConfigError::MissingApiKey => CliError::MissingApiKey {
                provider: cli.provider.name().to_string(),
                env,
            },
            other => CliError::Config(other),
        })?;
    let provider = build_provider(&model_config, &key)?;
    Ok(ProviderSelection {
        provider,
        model: model_config.model.clone(),
    })
}

/// Fake provider 选择（离线冒烟，无 key）。
fn fake_selection(cli: &Cli) -> ProviderSelection {
    ProviderSelection {
        provider: Arc::new(FakeProvider),
        model: cli
            .model
            .clone()
            .unwrap_or_else(|| cli.provider.default_model().to_string()),
    }
}

/// 配置路径的 api_key 错误映射：`MissingApiKey` → `CliError::MissingApiKey`
/// （保留友好提示），其余 → `CliError::Config`。
fn map_config_error(e: ProviderConfigError, model_config: &ModelConfig) -> CliError {
    match e {
        ProviderConfigError::MissingApiKey => CliError::MissingApiKey {
            provider: model_config.name.clone(),
            env: model_config.protocol.default_api_key_env(),
        },
        other => CliError::Config(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Command;

    fn base_cli() -> Cli {
        Cli {
            command: Some(Command::Acp),
            model: None,
            provider: Provider::Openai,
            session: None,
            cwd: None,
            log: None,
            api_key: None,
            base_url: None,
            system_prompt: None,
            config: None,
            api_key_env: None,
        }
    }

    /// 配置名命中：`-m <配置名>` → model id 来自配置（protocol/base_url/key 亦来自配置）。
    #[test]
    fn select_provider_config_name_hit() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("guigu.toml");
        std::fs::write(
            &config_path,
            r#"
[models.ollama]
protocol = "openai"
base_url = "http://localhost:11434/v1"
api_key = "ollama-key"
model = "llama3"
"#,
        )
        .unwrap();
        let mut cli = base_cli();
        cli.model = Some("ollama".to_string());
        cli.config = Some(config_path);
        let selection = select_provider(&cli).unwrap();
        assert_eq!(selection.model, "llama3");
    }

    /// 内联回退：`-m <内联id>` + 空配置 → 走内联路径，model id 用 `-m`。
    #[test]
    fn select_provider_inline_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("empty.toml");
        std::fs::write(&config_path, "").unwrap();
        let mut cli = base_cli();
        cli.model = Some("gpt-4o".to_string());
        cli.api_key = Some("sk-test".to_string());
        cli.base_url = Some("http://localhost:9999/v1".to_string());
        cli.config = Some(config_path);
        let selection = select_provider(&cli).unwrap();
        assert_eq!(selection.model, "gpt-4o");
    }

    /// 内联回退：`-m` 未指定 → 用协议默认 model id。
    #[test]
    fn select_provider_inline_default_model() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("empty.toml");
        std::fs::write(&config_path, "").unwrap();
        let mut cli = base_cli();
        cli.api_key = Some("sk-test".to_string());
        cli.config = Some(config_path);
        let selection = select_provider(&cli).unwrap();
        assert_eq!(selection.model, "gpt-4o-mini");
    }

    /// Fake 早退：无 key 也能构造（离线冒烟）。
    #[test]
    fn select_provider_fake_early_return() {
        let mut cli = base_cli();
        cli.provider = Provider::Fake;
        let selection = select_provider(&cli).unwrap();
        assert_eq!(selection.model, "fake-model");
    }
}
