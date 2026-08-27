//! Task 007 端到端测试：wiremock 本地 mock server 验证真实 HTTP 格式。
//!
//! 覆盖：
//! - OpenAI 完整 SSE 流 → 事件序列 + Done message（content/usage/stop_reason）
//! - Anthropic 完整 SSE 流 → 事件序列 + Done message
//! - HTTP 401 → `ProviderError::HttpStatus`
//! - 建立阶段取消 → `Err(ProviderError::Aborted)`
//!
//! 流内取消（`Error { aborted: true }`）由 `src/adapters/stream.rs` 单测覆盖
//! （wiremock 一次性发完 body，无法确定性地在流读取中途注入取消）。
#![cfg(feature = "providers-http")]

use futures::StreamExt;
use guigu::core::message::{AssistantContent, ThinkingLevel};
use guigu::core::provider::{
    AssistantEvent, Context, Model, ModelProvider, ProviderError, ProviderRequest,
};
use guigu::{AnthropicConfig, AnthropicProvider, OpenAiConfig, OpenAiProvider};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

fn base_url(server: &MockServer, suffix: &str) -> String {
    format!("http://{}{}", server.address(), suffix)
}

#[tokio::test]
async fn openai_full_stream_produces_events_and_done() {
    let server = MockServer::start().await;
    let sse = [
        "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2,\"total_tokens\":7}}\n\n",
        "data: [DONE]\n\n",
    ]
    .concat();
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .expect(1)
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new(OpenAiConfig {
        api_key: "sk-test".into(),
        base_url: Some(base_url(&server, "/v1")),
    })
    .expect("provider");

    let stream = provider
        .stream(make_request(CancellationToken::new()))
        .await
        .expect("stream established");
    let events: Vec<AssistantEvent> = stream.collect().await;

    assert_eq!(
        events[0],
        AssistantEvent::TextDelta {
            text: "Hello".into()
        }
    );
    assert_eq!(
        events[1],
        AssistantEvent::TextDelta {
            text: " world".into()
        }
    );
    assert_eq!(events.len(), 3);
    if let AssistantEvent::Done { message } = &events[2] {
        assert_eq!(
            message.content,
            vec![AssistantContent::Text {
                text: "Hello world".into()
            }]
        );
        assert_eq!(
            message.model,
            Some(guigu::core::message::ModelId("test-model".into()))
        );
        let usage = message.usage.as_ref().expect("usage present");
        assert_eq!(usage.input, 5);
        assert_eq!(usage.output, 2);
        assert_eq!(usage.total_tokens, 7);
        assert_eq!(
            message.stop_reason,
            Some(guigu::core::message::StopReason::Completed)
        );
    } else {
        panic!("expected Done as last event");
    }
}

#[tokio::test]
async fn openai_tool_call_stream() {
    let server = MockServer::start().await;
    let sse = [
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"search\",\"arguments\":\"\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"q\\\":\\\"rust\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    ]
    .concat();
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .expect(1)
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new(OpenAiConfig {
        api_key: "sk-test".into(),
        base_url: Some(base_url(&server, "/v1")),
    })
    .expect("provider");

    let stream = provider
        .stream(make_request(CancellationToken::new()))
        .await
        .expect("stream established");
    let events: Vec<AssistantEvent> = stream.collect().await;

    // ToolCallStart, ToolCallDelta, ToolCallEnd, Done
    assert_eq!(events.len(), 4);
    assert!(matches!(
        events[0],
        AssistantEvent::ToolCallStart {
            ref id,
            ref name,
            ..
        } if id == "call_1" && name == "search"
    ));
    assert!(matches!(
        events[1],
        AssistantEvent::ToolCallDelta {
            ref id,
            ref arguments_delta,
        } if id == "call_1" && arguments_delta == "{\"q\":\"rust\"}"
    ));
    assert_eq!(
        events[2],
        AssistantEvent::ToolCallEnd {
            id: "call_1".into()
        }
    );
    if let AssistantEvent::Done { message } = &events[3] {
        assert_eq!(
            message.content,
            vec![AssistantContent::ToolCall(guigu::core::message::ToolCall {
                id: "call_1".into(),
                name: "search".into(),
                arguments: "{\"q\":\"rust\"}".into(),
            })]
        );
    } else {
        panic!("expected Done");
    }
}

#[tokio::test]
async fn anthropic_full_stream_produces_events_and_done() {
    let server = MockServer::start().await;
    let sse = [
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude\",\"stop_reason\":null,\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":2}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    ]
    .concat();
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .expect(1)
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(AnthropicConfig {
        api_key: "sk-ant-test".into(),
        base_url: Some(base_url(&server, "/v1")),
        max_tokens: 1024,
        anthropic_version: "2023-06-01".into(),
    })
    .expect("provider");

    let stream = provider
        .stream(make_request(CancellationToken::new()))
        .await
        .expect("stream established");
    let events: Vec<AssistantEvent> = stream.collect().await;

    assert_eq!(
        events[0],
        AssistantEvent::TextDelta {
            text: "Hello".into()
        }
    );
    assert_eq!(
        events[1],
        AssistantEvent::TextDelta {
            text: " world".into()
        }
    );
    assert_eq!(events.len(), 3);
    if let AssistantEvent::Done { message } = &events[2] {
        assert_eq!(
            message.content,
            vec![AssistantContent::Text {
                text: "Hello world".into()
            }]
        );
        let usage = message.usage.as_ref().expect("usage present");
        // input 来自 message_start，output 来自 message_delta。
        assert_eq!(usage.input, 5);
        assert_eq!(usage.output, 2);
        assert_eq!(usage.total_tokens, 7);
        assert_eq!(
            message.stop_reason,
            Some(guigu::core::message::StopReason::Completed)
        );
    } else {
        panic!("expected Done as last event");
    }
}

#[tokio::test]
async fn openai_http_401_returns_http_status_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_string("invalid api key"))
        .expect(1)
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new(OpenAiConfig {
        api_key: "sk-bad".into(),
        base_url: Some(base_url(&server, "/v1")),
    })
    .expect("provider");

    let result = provider
        .stream(make_request(CancellationToken::new()))
        .await;
    match result {
        Err(ProviderError::HttpStatus { status, body }) => {
            assert_eq!(status, 401);
            assert_eq!(body, "invalid api key");
        }
        Err(other) => panic!("expected HttpStatus, got {other:?}"),
        Ok(_) => panic!("expected error, got stream"),
    }
}

#[tokio::test]
async fn openai_establishment_cancellation_returns_aborted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw("data: [DONE]\n\n", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new(OpenAiConfig {
        api_key: "sk-test".into(),
        base_url: Some(base_url(&server, "/v1")),
    })
    .expect("provider");

    // 建立前取消 → 专用取消语义 Aborted（runtime 据此不重试）。
    let signal = CancellationToken::new();
    signal.cancel();
    let result = provider.stream(make_request(signal)).await;
    assert!(
        matches!(result, Err(ProviderError::Aborted)),
        "expected Aborted, got {:?}",
        result.is_ok()
    );
}
