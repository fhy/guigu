//! 模型端点配置（Task 022）：serde 可反序列化的 `ModelConfig` + TOML 配置文件
//! 加载 + api_key 四段解析链。
//!
//! Feature 边界：
//! - 数据结构（`Protocol`/`ModelConfig`/`GuiguConfig`/`ProviderConfigError`）与
//!   `ModelConfig::resolve_api_key` **不** feature-gate（纯 serde + std），
//!   `default-features = false` 下仍可反序列化配置、自行实现 provider。
//! - `Config::load`/`Config::resolve`（TOML 解析）gate 在 `config` feature
//!   （依赖 `toml`）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// 模型协议。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    /// OpenAI 兼容协议。
    OpenAi,
    /// Anthropic 协议。
    Anthropic,
}

impl Protocol {
    /// 协议默认 API key 环境变量名（对齐 015 语义）。
    pub fn default_api_key_env(self) -> &'static str {
        match self {
            Protocol::OpenAi => "OPENAI_API_KEY",
            Protocol::Anthropic => "ANTHROPIC_API_KEY",
        }
    }
}

/// 模型端点配置（配置文件 `models` 表的一项）。
#[derive(Debug, Clone, Deserialize)]
pub struct ModelConfig {
    /// 配置键名（唯一标识），加载时由表键注入（TOML 中可省略）。
    #[serde(default)]
    pub name: String,
    /// 协议。
    pub protocol: Protocol,
    /// 覆盖 base URL（None → 协议默认端点）。
    #[serde(default)]
    pub base_url: Option<String>,
    /// 明文 API key（低优先级）。
    #[serde(default)]
    pub api_key: Option<String>,
    /// API key 环境变量名（中优先级）。
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// 默认 model id（透传给 provider 请求体）。
    pub model: String,
    /// `max_tokens`（Anthropic 用，缺省 4096）。
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// `anthropic-version`（Anthropic 用，缺省 "2023-06-01"）。
    #[serde(default)]
    pub anthropic_version: Option<String>,
}

impl ModelConfig {
    /// 解析 API key（四段优先级链，从高到低）：
    ///
    /// 1. `cli_key`（CLI `-k/--api-key`，最高）
    /// 2. `self.api_key`（配置文件明文）
    /// 3. `self.api_key_env` 指向的环境变量
    /// 4. 协议默认 env（`OPENAI_API_KEY` / `ANTHROPIC_API_KEY`）
    ///
    /// 最终为空 → [`ProviderConfigError::MissingApiKey`]。`api_key_env` 显式设置
    /// 但对应环境变量未设置（或为空）→ [`ProviderConfigError::ApiKeyEnvUnset`]
    /// （显式引用缺失视为配置错误，不回退，避免掩盖配置问题）。
    pub fn resolve_api_key(&self, cli_key: Option<&str>) -> Result<String, ProviderConfigError> {
        // 1. CLI -k（最高）。
        if let Some(key) = cli_key
            && !key.is_empty()
        {
            return Ok(key.to_string());
        }
        // 2. 配置文件明文。
        if let Some(key) = &self.api_key
            && !key.is_empty()
        {
            return Ok(key.clone());
        }
        // 3. api_key_env 指向的环境变量。
        if let Some(env) = &self.api_key_env {
            match std::env::var(env) {
                Ok(value) if !value.is_empty() => return Ok(value),
                _ => return Err(ProviderConfigError::ApiKeyEnvUnset(env.clone())),
            }
        }
        // 4. 协议默认 env。
        let default_env = self.protocol.default_api_key_env();
        if let Ok(value) = std::env::var(default_env)
            && !value.is_empty()
        {
            return Ok(value);
        }
        Err(ProviderConfigError::MissingApiKey)
    }
}

/// 顶层配置文件。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GuiguConfig {
    /// 模型配置表（键 = 配置名）。
    #[serde(default)]
    pub models: HashMap<String, ModelConfig>,
}

/// 配置错误。
#[derive(Debug, thiserror::Error)]
pub enum ProviderConfigError {
    /// 未知协议。
    #[error("unknown protocol: {0}")]
    UnknownProtocol(String),
    /// 缺少 API key（api_key 与 api_key_env 均未提供有效值）。
    #[error("missing api_key (neither api_key nor api_key_env set)")]
    MissingApiKey,
    /// api_key_env 指向的环境变量未设置。
    #[error("api_key_env `{0}` is not set")]
    ApiKeyEnvUnset(String),
    /// 配置解析错误（包装 toml/serde 错误）。
    #[error("config parse error: {0}")]
    Parse(String),
    /// provider 构建错误（包装 ProviderError）。
    #[error("build error: {0}")]
    Build(String),
}

/// 配置文件加载器（TOML 解析，gate 在 `config` feature）。
#[cfg(feature = "config")]
pub struct Config;

#[cfg(feature = "config")]
impl Config {
    /// 从指定路径加载配置：读文件 + `toml::from_str`，把每个 `models` 表键注入
    /// 为对应 `ModelConfig.name`。
    pub fn load(path: &Path) -> Result<GuiguConfig, ProviderConfigError> {
        let content =
            std::fs::read_to_string(path).map_err(|e| ProviderConfigError::Parse(e.to_string()))?;
        Self::parse(&content)
    }

    /// 解析 TOML 内容为 [`GuiguConfig`]（把表键注入为 `ModelConfig.name`）。
    fn parse(content: &str) -> Result<GuiguConfig, ProviderConfigError> {
        let mut config: GuiguConfig =
            toml::from_str(content).map_err(|e| ProviderConfigError::Parse(e.to_string()))?;
        for (key, model) in config.models.iter_mut() {
            model.name = key.clone();
        }
        Ok(config)
    }

    /// 按查找链解析配置：`explicit`（`--config`）→ `./guigu.toml` →
    /// `$XDG_CONFIG_HOME/guigu/config.toml`（缺省 `~/.config/guigu/config.toml`）
    /// → [`GuiguConfig::default`]（空配置，向后兼容）。
    ///
    /// 显式路径缺失 → 错误（用户显式指定）；隐式候选缺失 → 继续下一候选，
    /// 全部缺失 → 空配置（不报错）。
    pub fn resolve(explicit: Option<&Path>) -> Result<GuiguConfig, ProviderConfigError> {
        let cwd = std::env::current_dir().map_err(|e| ProviderConfigError::Parse(e.to_string()))?;
        let xdg = std::env::var("XDG_CONFIG_HOME").ok();
        let home = std::env::var("HOME").ok();
        match Self::resolve_path(explicit, &cwd, xdg.as_deref(), home.as_deref()) {
            Some(path) => Self::load(&path),
            None => Ok(GuiguConfig::default()),
        }
    }

    /// 按查找链解析配置文件路径（纯函数，便于单测）。
    fn resolve_path(
        explicit: Option<&Path>,
        cwd: &Path,
        xdg_config_home: Option<&str>,
        home: Option<&str>,
    ) -> Option<PathBuf> {
        // 1. 显式 --config（缺失由 load 报错）。
        if let Some(path) = explicit {
            return Some(path.to_path_buf());
        }
        // 2. ./guigu.toml.
        let local = cwd.join("guigu.toml");
        if local.exists() {
            return Some(local);
        }
        // 3. $XDG_CONFIG_HOME/guigu/config.toml 或 ~/.config/guigu/config.toml。
        let base = xdg_config_home
            .map(PathBuf::from)
            .or_else(|| home.map(|h| Path::new(h).join(".config")));
        if let Some(base) = base {
            let global = base.join("guigu").join("config.toml");
            if global.exists() {
                return Some(global);
            }
        }
        // 4. 无候选。
        None
    }
}

#[cfg(test)]
mod tests;
