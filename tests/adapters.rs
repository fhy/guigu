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

/// 带一条 user 消息的请求（用于请求 body 断言）。
fn make_request_with_user_message(signal: CancellationToken) -> ProviderRequest {
    ProviderRequest {
        model: Model {
            id: "test-model".into(),
            context_window: 128000,
        },
        context: Context {
            system_prompt: "be helpful".into(),
            messages: vec![guigu::core::message::Message::User(
                guigu::core::message::UserMessage {
                    content: vec![guigu::core::message::UserContent::Text {
                        text: "hello".into(),
                    }],
                    timestamp: 0,
                },
            )],
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

#[tokio::test]
async fn anthropic_interleaved_blocks_preserve_order() {
    // text(0) → tool_use(1) → text(2)：两个文本块独立，顺序保留。
    let server = MockServer::start().await;
    let sse = [
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Let me check.\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"search\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"q\\\":\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"rust\\\"}\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"text_delta\",\"text\":\"Done.\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":9}}\n\n",
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

    assert_eq!(events.len(), 7);
    assert_eq!(
        events[0],
        AssistantEvent::TextDelta {
            text: "Let me check.".into()
        }
    );
    assert!(matches!(
        events[1],
        AssistantEvent::ToolCallStart {
            ref id,
            ref name,
            ..
        } if id == "tu_1" && name == "search"
    ));
    assert_eq!(
        events[5],
        AssistantEvent::TextDelta {
            text: "Done.".into()
        }
    );
    if let AssistantEvent::Done { message } = &events[6] {
        assert_eq!(
            message.content,
            vec![
                AssistantContent::Text {
                    text: "Let me check.".into()
                },
                AssistantContent::ToolCall(guigu::core::message::ToolCall {
                    id: "tu_1".into(),
                    name: "search".into(),
                    arguments: "{\"q\":\"rust\"}".into(),
                }),
                AssistantContent::Text {
                    text: "Done.".into()
                },
            ]
        );
        assert_eq!(
            message.stop_reason,
            Some(guigu::core::message::StopReason::Completed)
        );
    } else {
        panic!("expected Done as last event");
    }
}

#[tokio::test]
async fn openai_non_contiguous_tool_indices() {
    // provider index 非连续（1 与 3）且交错续块：参数必须按 index 归位。
    let server = MockServer::start().await;
    let sse = [
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_a\",\"type\":\"function\",\"function\":{\"name\":\"a\",\"arguments\":\"\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":3,\"id\":\"call_b\",\"type\":\"function\",\"function\":{\"name\":\"b\",\"arguments\":\"\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"function\":{\"arguments\":\"{\\\"x\\\":\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":3,\"function\":{\"arguments\":\"{\\\"y\\\":\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"function\":{\"arguments\":\"1}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":3,\"function\":{\"arguments\":\"2}\"}}]}}]}\n\n",
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

    assert_eq!(events.len(), 9);
    if let AssistantEvent::Done { message } = &events[8] {
        assert_eq!(
            message.content,
            vec![
                AssistantContent::ToolCall(guigu::core::message::ToolCall {
                    id: "call_a".into(),
                    name: "a".into(),
                    arguments: "{\"x\":1}".into(),
                }),
                AssistantContent::ToolCall(guigu::core::message::ToolCall {
                    id: "call_b".into(),
                    name: "b".into(),
                    arguments: "{\"y\":2}".into(),
                }),
            ]
        );
    } else {
        panic!("expected Done as last event");
    }
}

#[tokio::test]
async fn openai_request_body_and_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw("data: [DONE]\n\n", "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new(OpenAiConfig {
        api_key: "sk-test".into(),
        base_url: Some(base_url(&server, "/v1")),
    })
    .expect("provider");
    let stream = provider
        .stream(make_request_with_user_message(CancellationToken::new()))
        .await
        .expect("stream established");
    let _: Vec<AssistantEvent> = stream.collect().await;

    let received = server.received_requests().await.expect("request recorded");
    assert_eq!(received.len(), 1);
    let req = &received[0];
    assert_eq!(
        req.headers
            .get("authorization")
            .map(|v| v.to_str().unwrap()),
        Some("Bearer sk-test")
    );
    let body: serde_json::Value = serde_json::from_slice(&req.body).expect("json body");
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
    let messages = body["messages"].as_array().expect("messages array");
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], "be helpful");
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"], "hello");
    assert!(body.get("tools").is_none(), "empty tools omitted");
}

#[tokio::test]
async fn anthropic_request_body_and_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
            "text/event-stream",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(AnthropicConfig {
        api_key: "sk-ant-test".into(),
        base_url: Some(base_url(&server, "/v1")),
        max_tokens: 2048,
        anthropic_version: "2024-01-01".into(),
    })
    .expect("provider");
    let stream = provider
        .stream(make_request_with_user_message(CancellationToken::new()))
        .await
        .expect("stream established");
    let _: Vec<AssistantEvent> = stream.collect().await;

    let received = server.received_requests().await.expect("request recorded");
    assert_eq!(received.len(), 1);
    let req = &received[0];
    assert_eq!(
        req.headers.get("x-api-key").map(|v| v.to_str().unwrap()),
        Some("sk-ant-test")
    );
    assert_eq!(
        req.headers
            .get("anthropic-version")
            .map(|v| v.to_str().unwrap()),
        Some("2024-01-01")
    );
    let body: serde_json::Value = serde_json::from_slice(&req.body).expect("json body");
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["max_tokens"], 2048);
    assert_eq!(body["system"], "be helpful");
    assert_eq!(body["stream"], true);
    let messages = body["messages"].as_array().expect("messages array");
    assert_eq!(messages[0]["role"], "user");
    let blocks = messages[0]["content"].as_array().expect("content blocks");
    assert_eq!(blocks[0]["type"], "text");
    assert_eq!(blocks[0]["text"], "hello");
    assert!(body.get("tools").is_none(), "empty tools omitted");
}

#[tokio::test]
async fn openai_unknown_tool_index_emits_stream_error() {
    // 续块 index 未登记 → 流内 Error（aborted: false），不发 Done。
    let server = MockServer::start().await;
    let sse = [
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"t\",\"arguments\":\"\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":5,\"function\":{\"arguments\":\"x\"}}]}}]}\n\n",
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

    // 未知 index → 恰好一个流内 Error（aborted: false）；不发 Done，参数增量不静默应用。
    let errors: Vec<&AssistantEvent> = events
        .iter()
        .filter(|e| matches!(e, AssistantEvent::Error { .. }))
        .collect();
    assert_eq!(errors.len(), 1, "exactly one stream error: {events:?}");
    assert!(
        matches!(&errors[0], AssistantEvent::Error { aborted, .. } if !aborted),
        "expected non-aborted error, got {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AssistantEvent::Done { .. })),
        "no Done after parse error"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AssistantEvent::ToolCallDelta { .. })),
        "unknown-index arguments must not be applied"
    );
}

#[tokio::test]
async fn anthropic_delta_unknown_block_emits_stream_error() {
    // delta 指向未 start 的 block index → 流内 Error（aborted: false），不发 Done。
    let server = MockServer::start().await;
    let sse = [
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":3,\"delta\":{\"type\":\"text_delta\",\"text\":\"x\"}}\n\n",
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

    assert_eq!(events.len(), 1);
    assert!(
        matches!(&events[0], AssistantEvent::Error { aborted, .. } if !aborted),
        "expected non-aborted stream error, got {events:?}"
    );
}
