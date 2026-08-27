use super::*;
use crate::core::message::{ImageContent, ModelId, StopReason, ThinkingLevel, ToolCall};
use crate::core::provider::{Context, Model};
use tokio_util::sync::CancellationToken;

fn make_request(system: &str, messages: Vec<Message>, tools: Vec<ToolSpec>) -> ProviderRequest {
    ProviderRequest {
        model: Model {
            id: "claude-3-5-sonnet".into(),
            context_window: 200000,
        },
        context: Context {
            system_prompt: system.into(),
            messages,
            tools,
        },
        thinking_level: ThinkingLevel::Off,
        session_id: None,
        signal: CancellationToken::new(),
    }
}

fn user_text(text: &str) -> Message {
    Message::User(UserMessage {
        content: vec![UserContent::Text { text: text.into() }],
        timestamp: 0,
    })
}

#[test]
fn url_and_headers() {
    let config = AnthropicConfig::new("sk-ant-test");
    let req = make_request("sys", vec![], vec![]);
    let built = build_request(&config, &req).expect("build");
    assert_eq!(built.url, "https://api.anthropic.com/v1/messages");
    assert!(
        built
            .headers
            .iter()
            .any(|(k, v)| k == "x-api-key" && v == "sk-ant-test")
    );
    assert!(
        built
            .headers
            .iter()
            .any(|(k, v)| k == "anthropic-version" && v == "2023-06-01")
    );
    assert!(
        built
            .headers
            .iter()
            .any(|(k, v)| k == "Content-Type" && v == "application/json")
    );
}

#[test]
fn custom_base_url_and_version() {
    let config = AnthropicConfig {
        api_key: "k".into(),
        base_url: Some("http://localhost:9999/v1".into()),
        max_tokens: 100,
        anthropic_version: "2024-01-01".into(),
    };
    let req = make_request("", vec![], vec![]);
    let built = build_request(&config, &req).expect("build");
    assert_eq!(built.url, "http://localhost:9999/v1/messages");
    assert_eq!(built.body["max_tokens"], 100);
    assert!(
        built
            .headers
            .iter()
            .any(|(k, v)| k == "anthropic-version" && v == "2024-01-01")
    );
}

#[test]
fn body_shape_with_system() {
    let config = AnthropicConfig::new("k");
    let req = make_request("be helpful", vec![user_text("hi")], vec![]);
    let built = build_request(&config, &req).expect("build");
    assert_eq!(built.body["model"], "claude-3-5-sonnet");
    assert_eq!(built.body["max_tokens"], 4096);
    assert_eq!(built.body["system"], "be helpful");
    assert_eq!(built.body["stream"], true);
    let msgs = built.body["messages"].as_array().expect("msgs");
    assert_eq!(msgs[0]["role"], "user");
    assert!(built.body.get("tools").is_none(), "empty tools omitted");
}

#[test]
fn tools_mapped_with_input_schema() {
    let tools = vec![ToolSpec {
        name: "search".into(),
        description: "search".into(),
        parameters: Some(json!({ "type": "object", "properties": {} })),
    }];
    let config = AnthropicConfig::new("k");
    let req = make_request("", vec![], tools);
    let built = build_request(&config, &req).expect("build");
    let tools_arr = built.body["tools"].as_array().expect("tools");
    assert_eq!(tools_arr[0]["name"], "search");
    assert_eq!(tools_arr[0]["description"], "search");
    assert_eq!(tools_arr[0]["input_schema"]["type"], "object");
}

#[test]
fn tool_without_parameters_uses_object_schema() {
    let tools = vec![ToolSpec {
        name: "t".into(),
        description: "d".into(),
        parameters: None,
    }];
    let config = AnthropicConfig::new("k");
    let req = make_request("", vec![], tools);
    let built = build_request(&config, &req).expect("build");
    assert_eq!(
        built.body["tools"][0]["input_schema"],
        json!({ "type": "object" })
    );
}

#[test]
fn user_text_uses_content_blocks() {
    let config = AnthropicConfig::new("k");
    let req = make_request("", vec![user_text("hello")], vec![]);
    let built = build_request(&config, &req).expect("build");
    let content = built.body["messages"][0]["content"]
        .as_array()
        .expect("blocks");
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "hello");
}

#[test]
fn user_with_image_block() {
    let msg = Message::User(UserMessage {
        content: vec![
            UserContent::Text {
                text: "look".into(),
            },
            UserContent::Image(ImageContent {
                data: "B64".into(),
                mime_type: "image/jpeg".into(),
            }),
        ],
        timestamp: 0,
    });
    let config = AnthropicConfig::new("k");
    let req = make_request("", vec![msg], vec![]);
    let built = build_request(&config, &req).expect("build");
    let content = built.body["messages"][0]["content"]
        .as_array()
        .expect("blocks");
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content[1]["source"]["type"], "base64");
    assert_eq!(content[1]["source"]["media_type"], "image/jpeg");
    assert_eq!(content[1]["source"]["data"], "B64");
}

#[test]
fn assistant_tool_use_input_parsed() {
    let msg = Message::Assistant(AssistantMessage {
        content: vec![
            AssistantContent::Text {
                text: "using tool".into(),
            },
            AssistantContent::ToolCall(ToolCall {
                id: "tu_1".into(),
                name: "search".into(),
                arguments: "{\"q\":\"rust\"}".into(),
            }),
        ],
        model: Some(ModelId("claude".into())),
        usage: None,
        stop_reason: Some(StopReason::Completed),
        error_message: None,
        timestamp: 0,
    });
    let config = AnthropicConfig::new("k");
    let req = make_request("", vec![msg], vec![]);
    let built = build_request(&config, &req).expect("build");
    let content = built.body["messages"][0]["content"]
        .as_array()
        .expect("blocks");
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[1]["type"], "tool_use");
    assert_eq!(content[1]["id"], "tu_1");
    assert_eq!(content[1]["name"], "search");
    assert_eq!(content[1]["input"]["q"], "rust");
}

#[test]
fn assistant_invalid_arguments_is_build_error() {
    let msg = Message::Assistant(AssistantMessage {
        content: vec![AssistantContent::ToolCall(ToolCall {
            id: "tu".into(),
            name: "n".into(),
            arguments: "not-json".into(),
        })],
        model: None,
        usage: None,
        stop_reason: None,
        error_message: None,
        timestamp: 0,
    });
    let config = AnthropicConfig::new("k");
    let req = make_request("", vec![msg], vec![]);
    // 非法 arguments 不得静默降级为 {}，必须返回 Build 错误。
    assert!(matches!(
        build_request(&config, &req),
        Err(ProviderError::Build(_))
    ));
}

#[test]
fn assistant_non_object_arguments_is_build_error() {
    let msg = Message::Assistant(AssistantMessage {
        content: vec![AssistantContent::ToolCall(ToolCall {
            id: "tu".into(),
            name: "n".into(),
            arguments: "[1,2]".into(),
        })],
        model: None,
        usage: None,
        stop_reason: None,
        error_message: None,
        timestamp: 0,
    });
    let config = AnthropicConfig::new("k");
    let req = make_request("", vec![msg], vec![]);
    assert!(matches!(
        build_request(&config, &req),
        Err(ProviderError::Build(_))
    ));
}

#[test]
fn assistant_empty_arguments_maps_to_empty_object() {
    let msg = Message::Assistant(AssistantMessage {
        content: vec![AssistantContent::ToolCall(ToolCall {
            id: "tu".into(),
            name: "n".into(),
            arguments: String::new(),
        })],
        model: None,
        usage: None,
        stop_reason: None,
        error_message: None,
        timestamp: 0,
    });
    let config = AnthropicConfig::new("k");
    let req = make_request("", vec![msg], vec![]);
    let built = build_request(&config, &req).expect("build");
    let content = built.body["messages"][0]["content"]
        .as_array()
        .expect("blocks");
    assert_eq!(content[0]["input"], json!({}));
}

#[test]
fn tool_result_is_user_message() {
    let msg = Message::ToolResult(ToolResultMessage {
        tool_call_id: "tu_1".into(),
        tool_name: "search".into(),
        is_error: true,
        content: vec![ToolResultContent::Text {
            text: "boom".into(),
        }],
        details: None,
        timestamp: 0,
    });
    let config = AnthropicConfig::new("k");
    let req = make_request("", vec![msg], vec![]);
    let built = build_request(&config, &req).expect("build");
    let m = &built.body["messages"][0];
    assert_eq!(m["role"], "user");
    let block = &m["content"][0];
    assert_eq!(block["type"], "tool_result");
    assert_eq!(block["tool_use_id"], "tu_1");
    assert_eq!(block["content"], "boom");
    assert_eq!(block["is_error"], true);
}
