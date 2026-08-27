//! Anthropic 请求构造（纯函数：`ProviderRequest` → URL + headers + JSON body）。

use serde_json::{Value, json};

use crate::core::message::{
    AssistantContent, AssistantMessage, Message, ToolResultContent, ToolResultMessage, UserContent,
    UserMessage,
};
use crate::core::provider::{ProviderError, ProviderRequest, ToolSpec};

use super::super::BuiltRequest;
use super::AnthropicConfig;

/// 默认 base URL。
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1";

/// 构造 Anthropic Messages 请求（纯函数）。
///
/// body 含 `model`/`max_tokens`/`messages`/`stream:true`；`system` 非空时加入顶层；
/// `tools` 为空时省略。
pub(crate) fn build_request(
    config: &AnthropicConfig,
    request: &ProviderRequest,
) -> Result<BuiltRequest, ProviderError> {
    let base = config
        .base_url
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_BASE_URL);
    let url = format!("{}/messages", base.trim_end_matches('/'));

    let mut messages = Vec::new();
    for msg in &request.context.messages {
        messages.push(map_message(msg));
    }

    let mut body = json!({
        "model": request.model.id,
        "max_tokens": config.max_tokens,
        "messages": messages,
        "stream": true,
    });
    if !request.context.system_prompt.is_empty() {
        body["system"] = json!(request.context.system_prompt);
    }
    if !request.context.tools.is_empty() {
        body["tools"] = json!(map_tools(&request.context.tools));
    }

    Ok(BuiltRequest {
        url,
        headers: vec![
            ("x-api-key".to_string(), config.api_key.clone()),
            (
                "anthropic-version".to_string(),
                config.anthropic_version.clone(),
            ),
            ("Content-Type".to_string(), "application/json".to_string()),
        ],
        body,
    })
}

/// 工具规格 → Anthropic `tools` 数组（`input_schema`）。
fn map_tools(tools: &[ToolSpec]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t
                    .parameters
                    .clone()
                    .unwrap_or_else(|| json!({ "type": "object" })),
            })
        })
        .collect()
}

/// 单条内部消息 → Anthropic 消息（content 为 content blocks 数组）。
fn map_message(msg: &Message) -> Value {
    match msg {
        Message::User(u) => map_user(u),
        Message::Assistant(a) => map_assistant(a),
        Message::ToolResult(t) => map_tool_result(t),
    }
}

/// User → Anthropic user 消息（content blocks：text / image）。
fn map_user(u: &UserMessage) -> Value {
    let blocks: Vec<Value> = u
        .content
        .iter()
        .map(|c| match c {
            UserContent::Text { text } => json!({ "type": "text", "text": text }),
            UserContent::Image(img) => json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": img.mime_type,
                    "data": img.data,
                }
            }),
        })
        .collect();
    json!({ "role": "user", "content": blocks })
}

/// Assistant → Anthropic assistant 消息（Thinking 段忽略；tool_use 的 input 由 arguments 反序列化）。
fn map_assistant(a: &AssistantMessage) -> Value {
    let mut blocks: Vec<Value> = Vec::new();
    for c in &a.content {
        match c {
            AssistantContent::Text { text } => {
                blocks.push(json!({ "type": "text", "text": text }));
            }
            AssistantContent::Thinking { .. } => {
                // Thinking 段忽略。
            }
            AssistantContent::ToolCall(tc) => {
                let input: Value =
                    serde_json::from_str(&tc.arguments).unwrap_or_else(|_| json!({}));
                blocks.push(json!({
                    "type": "tool_use",
                    "id": tc.id,
                    "name": tc.name,
                    "input": input,
                }));
            }
        }
    }
    json!({ "role": "assistant", "content": blocks })
}

/// ToolResult → 独立 user 消息（tool_result block）。
fn map_tool_result(t: &ToolResultMessage) -> Value {
    let text = t
        .content
        .iter()
        .filter_map(|c| match c {
            ToolResultContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    json!({
        "role": "user",
        "content": [{
            "type": "tool_result",
            "tool_use_id": t.tool_call_id,
            "content": text,
            "is_error": t.is_error,
        }]
    })
}

#[cfg(test)]
mod tests {
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
    fn assistant_invalid_arguments_fallback_empty_object() {
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
}
