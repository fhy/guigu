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
        messages.push(map_message(msg)?);
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
fn map_message(msg: &Message) -> Result<Value, ProviderError> {
    match msg {
        Message::User(u) => Ok(map_user(u)),
        Message::Assistant(a) => map_assistant(a),
        Message::ToolResult(t) => Ok(map_tool_result(t)),
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
///
/// 非法 arguments JSON 返回 `ProviderError::Build`（不静默降级为 `{}`，
/// 避免把数据损坏隐藏成"空参数"发给模型）。
fn map_assistant(a: &AssistantMessage) -> Result<Value, ProviderError> {
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
                let input = parse_tool_arguments(&tc.id, &tc.arguments)?;
                blocks.push(json!({
                    "type": "tool_use",
                    "id": tc.id,
                    "name": tc.name,
                    "input": input,
                }));
            }
        }
    }
    Ok(json!({ "role": "assistant", "content": blocks }))
}

/// arguments JSON 字符串 → `input` 对象。
///
/// 空串归一为 `{}`（无参数调用）；非空但非法 JSON 或非对象返回
/// `ProviderError::Build`，让上游数据损坏在构造期暴露。
fn parse_tool_arguments(id: &str, arguments: &str) -> Result<Value, ProviderError> {
    if arguments.is_empty() {
        return Ok(json!({}));
    }
    let value: Value = serde_json::from_str(arguments).map_err(|e| {
        ProviderError::Build(format!(
            "assistant tool call {id} has invalid JSON arguments: {e}"
        ))
    })?;
    if !value.is_object() {
        return Err(ProviderError::Build(format!(
            "assistant tool call {id} arguments must be a JSON object, got {}",
            value_type_name(&value)
        )));
    }
    Ok(value)
}

/// JSON 值类型名（用于错误诊断）。
fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
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
mod tests;
