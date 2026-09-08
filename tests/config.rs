//! Task 022 集成测试：配置解析 + 工厂构建 + wiremock base_url 覆盖。
//!
//! 覆盖：
//! - `Config::load`：TOML 反序列化 + 表键注入 `name` + 可选字段
//! - `Config::resolve`：显式路径加载
//! - `build_provider`：OpenAi/Anthropic 两条分支构造
//! - wiremock 本地 mock：断言 `base_url` 覆盖后请求打到自定义地址（复用 007
//!   测试模式，不依赖外网）
//!
//! 整个文件 gate 在 `config` + `providers-http`（default 均含）；
//! `--no-default-features` 下不编译（`Config`/工厂不存在）。

#![cfg(all(feature = "config", feature = "providers-http"))]

use std::path::Path;

use futures::StreamExt;
use guigu::config::{Config, ModelConfig, Protocol};
use guigu::core::message::ThinkingLevel;
use guigu::core::provider::{AssistantEvent, Context, Model, ProviderRequest};
use guigu::{GuiguConfig, build_provider};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// 写一个 TOML 配置文件到临时目录，返回路径。
fn write_config(dir: &tempfile::TempDir, name: &str, content: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, content).unwrap();
    path
}

/// `Config::load`：TOML 反序列化 + 表键注入 `name` + 可选字段解析。
#[test]
fn load_toml_parses_and_injects_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(
        &dir,
        "guigu.toml",
        r#"
[models.ollama]
protocol = "openai"
base_url = "http://localhost:11434/v1"
api_key = "ollama"
model = "llama3"

[models.claude]
protocol = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
model = "claude-3-5-sonnet"
max_tokens = 8192
anthropic_version = "2023-06-01"
"#,
    );
    let config: GuiguConfig = Config::load(&path).unwrap();

    let ollama = config.models.get("ollama").expect("ollama entry");
    assert_eq!(ollama.name, "ollama");
    assert_eq!(ollama.protocol, Protocol::OpenAi);
    assert_eq!(
        ollama.base_url.as_deref(),
        Some("http://localhost:11434/v1")
    );
    assert_eq!(ollama.api_key.as_deref(), Some("ollama"));
    assert_eq!(ollama.api_key_env, None);
    assert_eq!(ollama.model, "llama3");
    assert_eq!(ollama.max_tokens, None);
    assert_eq!(ollama.anthropic_version, None);

    let claude = config.models.get("claude").expect("claude entry");
    assert_eq!(claude.name, "claude");
    assert_eq!(claude.protocol, Protocol::Anthropic);
    assert_eq!(claude.base_url, None);
    assert_eq!(claude.api_key_env.as_deref(), Some("ANTHROPIC_API_KEY"));
    assert_eq!(claude.model, "claude-3-5-sonnet");
    assert_eq!(claude.max_tokens, Some(8192));
    assert_eq!(claude.anthropic_version.as_deref(), Some("2023-06-01"));
}

/// `Config::load`：空文件 → 空配置（向后兼容）。
#[test]
fn load_empty_toml_gives_empty_config() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(&dir, "empty.toml", "");
    let config = Config::load(&path).unwrap();
    assert!(config.models.is_empty());
}

/// `Config::resolve`：显式路径加载。
#[test]
fn resolve_explicit_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(
        &dir,
        "guigu.toml",
        r#"
[models.local]
protocol = "openai"
model = "m"
api_key = "k"
"#,
    );
    let config = Config::resolve(Some(Path::new(path.to_str().unwrap()))).unwrap();
    assert!(config.models.contains_key("local"));
    assert_eq!(config.models["local"].name, "local");
}

/// `build_provider`：OpenAi 分支构造成功。
#[test]
fn build_provider_openai() {
    let config = ModelConfig {
        name: "t".into(),
        protocol: Protocol::OpenAi,
        base_url: None,
        api_key: None,
        api_key_env: None,
        model: "gpt-4o".into(),
        max_tokens: None,
        anthropic_version: None,
    };
    let provider = build_provider(&config, "sk-test").unwrap();
    assert!(std::sync::Arc::strong_count(&provider) >= 1);
}

/// `build_provider`：Anthropic 分支构造成功（含 max_tokens/version 透传）。
#[test]
fn build_provider_anthropic() {
    let config = ModelConfig {
        name: "t".into(),
        protocol: Protocol::Anthropic,
        base_url: None,
        api_key: None,
        api_key_env: None,
        model: "claude-3".into(),
        max_tokens: Some(2048),
        anthropic_version: Some("2024-01-01".into()),
    };
    let provider = build_provider(&config, "sk-test").unwrap();
    assert!(std::sync::Arc::strong_count(&provider) >= 1);
}

fn make_request(signal: CancellationToken) -> ProviderRequest {
    ProviderRequest {
        model: Model {
            id: "test-model".into(),
            context_window: 128000,
        },
        context: Context {
            system_prompt: "sys".into(),
            messages: vec![],
            tools: vec![],
        },
        thinking_level: ThinkingLevel::Off,
        session_id: None,
        signal,
    }
}

/// wiremock：`base_url` 覆盖后，OpenAI 请求打到自定义地址（非默认 api.openai.com）。
#[tokio::test]
async fn base_url_override_routes_to_custom_endpoint() {
    let server = MockServer::start().await;
    let sse = [
        "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    ]
    .concat();
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .expect(1)
        .mount(&server)
        .await;

    let config = ModelConfig {
        name: "custom".into(),
        protocol: Protocol::OpenAi,
        base_url: Some(format!("http://{}/v1", server.address())),
        api_key: None,
        api_key_env: None,
        model: "test-model".into(),
        max_tokens: None,
        anthropic_version: None,
    };
    let provider = build_provider(&config, "sk-test").unwrap();
    let stream = provider
        .stream(make_request(CancellationToken::new()))
        .await
        .expect("stream established");
    let events: Vec<AssistantEvent> = stream.collect().await;
    // 请求打到自定义地址（mock 收到），且流正常结束。
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AssistantEvent::Done { .. })),
        "expected Done event, got: {events:?}"
    );
}

/// wiremock：Anthropic `base_url` 覆盖后，请求打到自定义地址。
#[tokio::test]
async fn anthropic_base_url_override_routes_to_custom_endpoint() {
    let server = MockServer::start().await;
    let sse = [
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"test-model\",\"stop_reason\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":1}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    ]
    .concat();
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .expect(1)
        .mount(&server)
        .await;

    let config = ModelConfig {
        name: "custom".into(),
        protocol: Protocol::Anthropic,
        base_url: Some(format!("http://{}/v1", server.address())),
        api_key: None,
        api_key_env: None,
        model: "test-model".into(),
        max_tokens: None,
        anthropic_version: None,
    };
    let provider = build_provider(&config, "sk-test").unwrap();
    let stream = provider
        .stream(make_request(CancellationToken::new()))
        .await
        .expect("stream established");
    let events: Vec<AssistantEvent> = stream.collect().await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AssistantEvent::Done { .. })),
        "expected Done event, got: {events:?}"
    );
}
