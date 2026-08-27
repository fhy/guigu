//! OpenAI 请求构造（纯函数：`ProviderRequest` → URL + headers + JSON body）。

use serde_json::{Value, json};

use crate::core::message::{
    AssistantContent, AssistantMessage, Message, ToolResultContent, ToolResultMessage, UserContent,
    UserMessage,
};
use crate::core::provider::{ProviderError, ProviderRequest, ToolSpec};

use super::super::BuiltRequest;
use super::OpenAiConfig;

/// 默认 base URL。
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// 构造 OpenAI Chat Completions 请求（纯函数）。
///
/// body 含 `model`/`messages`/`stream:true`/`stream_options.include_usage`；
/// `tools` 为空时省略。
pub(crate) fn build_request(
    config: &OpenAiConfig,
    request: &ProviderRequest,
) -> Result<BuiltRequest, ProviderError> {
    let base = config
        .base_url
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_BASE_URL);
    let url = format!("{}/chat/completions", base.trim_end_matches('/'));

    let mut messages = Vec::new();
    if !request.context.system_prompt.is_empty() {
        messages.push(json!({
            "role": "system",
            "content": request.context.system_prompt,
        }));
    }
    for msg in &request.context.messages {
        messages.push(map_message(msg));
    }

    let mut body = json!({
        "model": request.model.id,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    if !request.context.tools.is_empty() {
        body["tools"] = json!(map_tools(&request.context.tools));
    }

    Ok(BuiltRequest {
        url,
        headers: vec![
            (
                "Authorization".to_string(),
                format!("Bearer {}", config.api_key),
            ),
            ("Content-Type".to_string(), "application/json".to_string()),
        ],
        body,
    })
}

/// 工具规格 → OpenAI `tools` 数组。
fn map_tools(tools: &[ToolSpec]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters.clone().unwrap_or_else(|| json!({})),
                }
            })
        })
        .collect()
}

/// 单条内部消息 → OpenAI 消息。
fn map_message(msg: &Message) -> Value {
    match msg {
        Message::User(u) => map_user(u),
        Message::Assistant(a) => map_assistant(a),
        Message::ToolResult(t) => map_tool_result(t),
    }
}

/// User → OpenAI user 消息（纯文本拼接 / 含 Image 用 content 数组）。
fn map_user(u: &UserMessage) -> Value {
    let has_image = u.content.iter().any(|c| matches!(c, UserContent::Image(_)));
    if !has_image {
        let text = u
            .content
            .iter()
            .filter_map(|c| match c {
                UserContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");
        return json!({ "role": "user", "content": text });
    }
    let parts: Vec<Value> = u
        .content
        .iter()
        .map(|c| match c {
            UserContent::Text { text } => json!({ "type": "text", "text": text }),
            UserContent::Image(img) => json!({
                "type": "image_url",
                "image_url": {
                    "url": format!("data:{};base64,{}", img.mime_type, img.data)
                }
            }),
        })
        .collect();
    json!({ "role": "user", "content": parts })
}

/// Assistant → OpenAI assistant 消息（Thinking 段忽略；content 为拼接 Text 或 null）。
fn map_assistant(a: &AssistantMessage) -> Value {
    let text: String = a
        .content
        .iter()
        .filter_map(|c| match c {
            AssistantContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    let tool_calls: Vec<Value> = a
        .content
        .iter()
        .filter_map(|c| match c {
            AssistantContent::ToolCall(tc) => Some(json!({
                "id": tc.id,
                "type": "function",
                "function": { "name": tc.name, "arguments": tc.arguments },
            })),
            _ => None,
        })
        .collect();

    let mut obj = serde_json::Map::new();
    obj.insert("role".into(), json!("assistant"));
    obj.insert(
        "content".into(),
        if text.is_empty() {
            Value::Null
        } else {
            json!(text)
        },
    );
    if !tool_calls.is_empty() {
        obj.insert("tool_calls".into(), json!(tool_calls));
    }
    Value::Object(obj)
}

/// ToolResult → OpenAI tool 消息（拼接 Text）。
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
        "role": "tool",
        "tool_call_id": t.tool_call_id,
        "content": text,
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
                id: "gpt-4o".into(),
                context_window: 128000,
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
        let config = OpenAiConfig::new("sk-test");
        let req = make_request("sys", vec![], vec![]);
        let built = build_request(&config, &req).expect("build");
        assert_eq!(built.url, "https://api.openai.com/v1/chat/completions");
        assert!(
            built
                .headers
                .iter()
                .any(|(k, v)| k == "Authorization" && v == "Bearer sk-test")
        );
        assert!(
            built
                .headers
                .iter()
                .any(|(k, v)| k == "Content-Type" && v == "application/json")
        );
    }

    #[test]
    fn custom_base_url_trims_slash() {
        let config = OpenAiConfig {
            api_key: "k".into(),
            base_url: Some("http://localhost:9999/v1/".into()),
        };
        let req = make_request("", vec![], vec![]);
        let built = build_request(&config, &req).expect("build");
        assert_eq!(built.url, "http://localhost:9999/v1/chat/completions");
    }

    #[test]
    fn body_shape_with_system_and_stream_options() {
        let config = OpenAiConfig::new("k");
        let req = make_request("be nice", vec![user_text("hi")], vec![]);
        let built = build_request(&config, &req).expect("build");
        assert_eq!(built.body["model"], "gpt-4o");
        assert_eq!(built.body["stream"], true);
        assert_eq!(built.body["stream_options"]["include_usage"], true);
        let msgs = built.body["messages"].as_array().expect("msgs array");
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "be nice");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "hi");
        assert!(built.body.get("tools").is_none(), "empty tools omitted");
    }

    #[test]
    fn tools_mapped() {
        let tools = vec![ToolSpec {
            name: "search".into(),
            description: "search the web".into(),
            parameters: Some(json!({ "type": "object" })),
        }];
        let config = OpenAiConfig::new("k");
        let req = make_request("", vec![], tools);
        let built = build_request(&config, &req).expect("build");
        let tools_arr = built.body["tools"].as_array().expect("tools array");
        assert_eq!(tools_arr[0]["type"], "function");
        assert_eq!(tools_arr[0]["function"]["name"], "search");
        assert_eq!(tools_arr[0]["function"]["description"], "search the web");
        assert_eq!(tools_arr[0]["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn tool_without_parameters_uses_empty_object() {
        let tools = vec![ToolSpec {
            name: "t".into(),
            description: "d".into(),
            parameters: None,
        }];
        let config = OpenAiConfig::new("k");
        let req = make_request("", vec![], tools);
        let built = build_request(&config, &req).expect("build");
        assert_eq!(built.body["tools"][0]["function"]["parameters"], json!({}));
    }

    #[test]
    fn user_with_image_uses_content_array() {
        let msg = Message::User(UserMessage {
            content: vec![
                UserContent::Text {
                    text: "what is this".into(),
                },
                UserContent::Image(ImageContent {
                    data: "BASE64".into(),
                    mime_type: "image/png".into(),
                }),
            ],
            timestamp: 0,
        });
        let config = OpenAiConfig::new("k");
        let req = make_request("", vec![msg], vec![]);
        let built = build_request(&config, &req).expect("build");
        let content = built.body["messages"][0]["content"]
            .as_array()
            .expect("array");
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "what is this");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(
            content[1]["image_url"]["url"],
            "data:image/png;base64,BASE64"
        );
    }

    #[test]
    fn assistant_with_text_and_tool_call() {
        let msg = Message::Assistant(AssistantMessage {
            content: vec![
                AssistantContent::Text {
                    text: "let me search".into(),
                },
                AssistantContent::ToolCall(ToolCall {
                    id: "call_1".into(),
                    name: "search".into(),
                    arguments: "{\"q\":\"rust\"}".into(),
                }),
            ],
            model: Some(ModelId("gpt-4o".into())),
            usage: None,
            stop_reason: Some(StopReason::Completed),
            error_message: None,
            timestamp: 0,
        });
        let config = OpenAiConfig::new("k");
        let req = make_request("", vec![msg], vec![]);
        let built = build_request(&config, &req).expect("build");
        let m = &built.body["messages"][0];
        assert_eq!(m["role"], "assistant");
        assert_eq!(m["content"], "let me search");
        assert_eq!(m["tool_calls"][0]["id"], "call_1");
        assert_eq!(m["tool_calls"][0]["type"], "function");
        assert_eq!(m["tool_calls"][0]["function"]["name"], "search");
        assert_eq!(
            m["tool_calls"][0]["function"]["arguments"],
            "{\"q\":\"rust\"}"
        );
    }

    #[test]
    fn assistant_without_text_has_null_content() {
        let msg = Message::Assistant(AssistantMessage {
            content: vec![AssistantContent::ToolCall(ToolCall {
                id: "c".into(),
                name: "n".into(),
                arguments: "{}".into(),
            })],
            model: None,
            usage: None,
            stop_reason: None,
            error_message: None,
            timestamp: 0,
        });
        let config = OpenAiConfig::new("k");
        let req = make_request("", vec![msg], vec![]);
        let built = build_request(&config, &req).expect("build");
        assert!(built.body["messages"][0]["content"].is_null());
    }

    #[test]
    fn tool_result_mapped() {
        let msg = Message::ToolResult(ToolResultMessage {
            tool_call_id: "call_1".into(),
            tool_name: "search".into(),
            is_error: false,
            content: vec![ToolResultContent::Text {
                text: "result here".into(),
            }],
            details: None,
            timestamp: 0,
        });
        let config = OpenAiConfig::new("k");
        let req = make_request("", vec![msg], vec![]);
        let built = build_request(&config, &req).expect("build");
        let m = &built.body["messages"][0];
        assert_eq!(m["role"], "tool");
        assert_eq!(m["tool_call_id"], "call_1");
        assert_eq!(m["content"], "result here");
    }
}
