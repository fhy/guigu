//! 离线 fake provider（Task 015）：测试冒烟用，避免依赖外网。
//!
//! 规格验收要求「测试用 fake/offline provider 冒烟，避免依赖外网」。CLI 二进制
//! 默认只装配真 provider（openai/anthropic），无法满足离线冒烟，故提供隐藏的
//! `--provider fake`：固定回复单文本 turn（`TextDelta` + `Done`，
//! `stop_reason: Completed`），不发任何网络请求。`--help` 不显示（`hide = true`）。

use async_trait::async_trait;
use futures::stream;

use guigu::core::message::{AssistantContent, AssistantMessage, StopReason};
use guigu::core::provider::{
    AssistantEvent, AssistantStream, ModelProvider, ProviderError, ProviderRequest,
};

/// 离线 fake provider：固定回复 "ok"（单文本 turn，无网络）。
pub struct FakeProvider;

#[async_trait]
impl ModelProvider for FakeProvider {
    async fn stream(&self, _request: ProviderRequest) -> Result<AssistantStream, ProviderError> {
        let message = AssistantMessage {
            content: vec![AssistantContent::Text {
                text: "ok".to_string(),
            }],
            model: None,
            usage: None,
            stop_reason: Some(StopReason::Completed),
            error_message: None,
            timestamp: 0,
        };
        Ok(Box::pin(stream::iter(vec![
            AssistantEvent::TextDelta {
                text: "ok".to_string(),
            },
            AssistantEvent::Done { message },
        ])))
    }
}
