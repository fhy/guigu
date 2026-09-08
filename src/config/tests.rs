//! `config` 模块单元测试（Task 022）。

use super::*;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// 串行化 env 相关测试（`std::env` 进程级全局，避免并发互扰）。
static ENV_MUTEX: Mutex<()> = Mutex::new(());

fn lock_env() -> MutexGuard<'static, ()> {
    ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner())
}

/// RAII：设置/移除 env 变量，drop 时恢复原值。
struct EnvGuard {
    name: String,
    original: Option<String>,
}

impl EnvGuard {
    fn set(name: &str, value: &str) -> Self {
        let original = std::env::var(name).ok();
        // SAFETY: 测试经 `ENV_MUTEX` 串行化，无并发读写 env。
        unsafe { std::env::set_var(name, value) };
        Self {
            name: name.to_string(),
            original,
        }
    }
    fn remove(name: &str) -> Self {
        let original = std::env::var(name).ok();
        // SAFETY: 测试经 `ENV_MUTEX` 串行化，无并发读写 env。
        unsafe { std::env::remove_var(name) };
        Self {
            name: name.to_string(),
            original,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: 测试经 `ENV_MUTEX` 串行化，无并发读写 env。
        match &self.original {
            Some(value) => unsafe { std::env::set_var(&self.name, value) },
            None => unsafe { std::env::remove_var(&self.name) },
        }
    }
}

fn model_config(
    protocol: Protocol,
    api_key: Option<&str>,
    api_key_env: Option<&str>,
) -> ModelConfig {
    ModelConfig {
        name: "test".into(),
        protocol,
        base_url: None,
        api_key: api_key.map(str::to_string),
        api_key_env: api_key_env.map(str::to_string),
        model: "m".into(),
        max_tokens: None,
        anthropic_version: None,
    }
}

#[test]
fn protocol_serde_lowercase() {
    let p: Protocol = serde_json::from_str("\"openai\"").unwrap();
    assert_eq!(p, Protocol::OpenAi);
    let p: Protocol = serde_json::from_str("\"anthropic\"").unwrap();
    assert_eq!(p, Protocol::Anthropic);
}

#[test]
fn protocol_default_api_key_env() {
    assert_eq!(Protocol::OpenAi.default_api_key_env(), "OPENAI_API_KEY");
    assert_eq!(
        Protocol::Anthropic.default_api_key_env(),
        "ANTHROPIC_API_KEY"
    );
}

#[test]
fn model_config_deserialize_minimal() {
    let json = r#"{"protocol":"openai","model":"gpt-4o"}"#;
    let c: ModelConfig = serde_json::from_str(json).unwrap();
    assert_eq!(c.name, "");
    assert_eq!(c.protocol, Protocol::OpenAi);
    assert_eq!(c.model, "gpt-4o");
    assert_eq!(c.base_url, None);
    assert_eq!(c.api_key, None);
    assert_eq!(c.api_key_env, None);
    assert_eq!(c.max_tokens, None);
    assert_eq!(c.anthropic_version, None);
}

#[test]
fn model_config_deserialize_full() {
    let json = r#"{
        "name":"ollama",
        "protocol":"anthropic",
        "base_url":"http://localhost:8080",
        "api_key":"sk-test",
        "api_key_env":"MY_KEY",
        "model":"claude-3",
        "max_tokens":1024,
        "anthropic_version":"2024-01-01"
    }"#;
    let c: ModelConfig = serde_json::from_str(json).unwrap();
    assert_eq!(c.name, "ollama");
    assert_eq!(c.protocol, Protocol::Anthropic);
    assert_eq!(c.base_url.as_deref(), Some("http://localhost:8080"));
    assert_eq!(c.api_key.as_deref(), Some("sk-test"));
    assert_eq!(c.api_key_env.as_deref(), Some("MY_KEY"));
    assert_eq!(c.model, "claude-3");
    assert_eq!(c.max_tokens, Some(1024));
    assert_eq!(c.anthropic_version.as_deref(), Some("2024-01-01"));
}

#[test]
fn guigu_config_deserialize_empty() {
    let c: GuiguConfig = serde_json::from_str("{}").unwrap();
    assert!(c.models.is_empty());
}

#[test]
fn resolve_api_key_cli_key_highest() {
    let c = model_config(Protocol::OpenAi, Some("config-key"), None);
    assert_eq!(c.resolve_api_key(Some("cli-key")).unwrap(), "cli-key");
}

#[test]
fn resolve_api_key_config_key_when_no_cli() {
    let c = model_config(Protocol::OpenAi, Some("config-key"), None);
    assert_eq!(c.resolve_api_key(None).unwrap(), "config-key");
}

#[test]
fn resolve_api_key_cli_empty_falls_through() {
    let c = model_config(Protocol::OpenAi, Some("config-key"), None);
    assert_eq!(c.resolve_api_key(Some("")).unwrap(), "config-key");
}

#[test]
fn resolve_api_key_api_key_env() {
    let _lock = lock_env();
    let _guard = EnvGuard::set("GUIGU_TEST_KEY_1", "env-key");
    let c = model_config(Protocol::OpenAi, None, Some("GUIGU_TEST_KEY_1"));
    assert_eq!(c.resolve_api_key(None).unwrap(), "env-key");
}

#[test]
fn resolve_api_key_api_key_env_unset() {
    let _lock = lock_env();
    let _guard = EnvGuard::remove("GUIGU_TEST_KEY_2");
    let c = model_config(Protocol::OpenAi, None, Some("GUIGU_TEST_KEY_2"));
    assert!(matches!(
        c.resolve_api_key(None),
        Err(ProviderConfigError::ApiKeyEnvUnset(ref e)) if e == "GUIGU_TEST_KEY_2"
    ));
}

#[test]
fn resolve_api_key_api_key_env_beats_default_env() {
    let _lock = lock_env();
    let _g1 = EnvGuard::set("GUIGU_TEST_KEY_3", "env-key");
    let _g2 = EnvGuard::set("OPENAI_API_KEY", "default-key");
    let c = model_config(Protocol::OpenAi, None, Some("GUIGU_TEST_KEY_3"));
    assert_eq!(c.resolve_api_key(None).unwrap(), "env-key");
}

#[test]
fn resolve_api_key_protocol_default_env() {
    let _lock = lock_env();
    let _guard = EnvGuard::set("OPENAI_API_KEY", "default-env-key");
    let c = model_config(Protocol::OpenAi, None, None);
    assert_eq!(c.resolve_api_key(None).unwrap(), "default-env-key");
}

#[test]
fn resolve_api_key_missing() {
    let _lock = lock_env();
    let _guard = EnvGuard::remove("OPENAI_API_KEY");
    let c = model_config(Protocol::OpenAi, None, None);
    assert!(matches!(
        c.resolve_api_key(None),
        Err(ProviderConfigError::MissingApiKey)
    ));
}

#[cfg(feature = "config")]
#[test]
fn config_load_injects_name_from_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("guigu.toml");
    std::fs::write(
        &path,
        r#"
[models.ollama]
protocol = "openai"
base_url = "http://localhost:11434/v1"
model = "llama3"
"#,
    )
    .unwrap();
    let config = Config::load(&path).unwrap();
    let m = config.models.get("ollama").expect("ollama entry");
    assert_eq!(m.name, "ollama");
    assert_eq!(m.protocol, Protocol::OpenAi);
    assert_eq!(m.base_url.as_deref(), Some("http://localhost:11434/v1"));
    assert_eq!(m.model, "llama3");
}

#[cfg(feature = "config")]
#[test]
fn config_load_missing_file_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nope.toml");
    assert!(matches!(
        Config::load(&path),
        Err(ProviderConfigError::Parse(_))
    ));
}

#[cfg(feature = "config")]
#[test]
fn config_load_parse_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.toml");
    std::fs::write(&path, "not = valid toml [[[").unwrap();
    assert!(matches!(
        Config::load(&path),
        Err(ProviderConfigError::Parse(_))
    ));
}

#[cfg(feature = "config")]
#[test]
fn resolve_path_explicit_wins() {
    let p = Path::new("/explicit/guigu.toml");
    let resolved = Config::resolve_path(Some(p), Path::new("/cwd"), Some("/xdg"), Some("/home"));
    assert_eq!(resolved, Some(PathBuf::from("/explicit/guigu.toml")));
}

#[cfg(feature = "config")]
#[test]
fn resolve_path_local_when_exists() {
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("guigu.toml");
    std::fs::write(&local, "").unwrap();
    let resolved = Config::resolve_path(None, dir.path(), None, None);
    assert_eq!(resolved, Some(local));
}

#[cfg(feature = "config")]
#[test]
fn resolve_path_xdg_when_exists() {
    let dir = tempfile::tempdir().unwrap();
    let global = dir.path().join("guigu").join("config.toml");
    std::fs::create_dir_all(global.parent().unwrap()).unwrap();
    std::fs::write(&global, "").unwrap();
    let xdg = dir.path().to_str().unwrap();
    let resolved = Config::resolve_path(None, Path::new("/nonexistent-cwd"), Some(xdg), None);
    assert_eq!(resolved, Some(global));
}

#[cfg(feature = "config")]
#[test]
fn resolve_path_none_when_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = Config::resolve_path(None, dir.path(), None, None);
    assert_eq!(resolved, None);
}
