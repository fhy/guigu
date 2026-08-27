//! OpenAI 兼容 Chat Completions API 适配器。
//!
//! 请求构造见 [`request`]，事件映射见 [`events`]，流逻辑复用 [`crate::adapters::stream`]。

mod events;
mod request;

use async_trait::async_trait;
use futures::StreamExt;

use crate::core::provider::{AssistantStream, ModelProvider, ProviderError, ProviderRequest};

use super::stream::build_stream;

pub use request::DEFAULT_BASE_URL;

/// OpenAI 适配器配置。
#[derive(Debug, Clone)]
pub struct OpenAiConfig {
    /// API key（`Authorization: Bearer`）。
    pub api_key: String,
    /// 覆盖 base URL（默认 [`DEFAULT_BASE_URL`]）。
    pub base_url: Option<String>,
}

impl OpenAiConfig {
    /// 新建配置（默认 base URL）。
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: None,
        }
    }
}

/// OpenAI 兼容 provider。
#[derive(Debug)]
pub struct OpenAiProvider {
    client: reqwest::Client,
    config: OpenAiConfig,
}

impl OpenAiProvider {
    /// 新建 provider（构造 reqwest Client）。
    pub fn new(config: OpenAiConfig) -> Result<Self, ProviderError> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| ProviderError::Build(e.to_string()))?;
        Ok(Self { client, config })
    }
}

#[async_trait]
impl ModelProvider for OpenAiProvider {
    async fn stream(&self, request: ProviderRequest) -> Result<AssistantStream, ProviderError> {
        let built = request::build_request(&self.config, &request)?;
        let mut req = self.client.post(&built.url);
        for (key, value) in &built.headers {
            req = req.header(key, value);
        }
        let req = req.json(&built.body);

        // 建立请求阶段：取消 → Aborted（专用取消语义，runtime 据此不重试）。
        let response = tokio::select! {
            res = req.send() => res.map_err(|e| ProviderError::Network(e.to_string()))?,
            _ = request.signal.cancelled() => return Err(ProviderError::Aborted),
        };

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response
                .text()
                .await
                .map_err(|e| ProviderError::Network(e.to_string()))?;
            return Err(ProviderError::HttpStatus { status, body });
        }

        let body = response.bytes_stream().map(|r| r.map(|b| b.to_vec()));
        Ok(build_stream(
            body,
            request.signal,
            request.model.id,
            events::map_event,
        ))
    }
}
