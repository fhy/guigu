//! OpenAI 事件映射（纯函数：SSE `data` chunk → `AssistantEvent` + 累积）。
//!
//! chunk 形状：`{choices:[{delta,finish_reason}],usage}`。

use serde_json::Value;

use crate::core::message::{StopReason, Usage};
use crate::core::provider::{AssistantEvent, ProviderError};

use super::super::acc::Acc;
use super::super::sse::SseEvent;

/// 映射一个 SSE 事件为 `AssistantEvent` 列表（并更新 [`Acc`]）。
pub(crate) fn map_event(
    sse: SseEvent,
    acc: &mut Acc,
) -> Result<Vec<AssistantEvent>, ProviderError> {
    match sse {
        SseEvent::Data { data } | SseEvent::Named { data, .. } => map_chunk(&data, acc),
        SseEvent::Done => Ok(vec![AssistantEvent::Done {
            message: acc.build_message(),
        }]),
    }
}

/// 映射单个 chunk。
fn map_chunk(data: &str, acc: &mut Acc) -> Result<Vec<AssistantEvent>, ProviderError> {
    let v: Value = serde_json::from_str(data).map_err(|e| ProviderError::Parse(e.to_string()))?;
    let mut events = Vec::new();

    if let Some(choices) = v.get("choices").and_then(|c| c.as_array())
        && let Some(choice) = choices.first()
    {
        let delta = &choice["delta"];

        // 文本增量。
        if let Some(text) = delta.get("content").and_then(|c| c.as_str())
            && !text.is_empty()
        {
            acc.append_text(text);
            events.push(AssistantEvent::TextDelta {
                text: text.to_string(),
            });
        }

        // 思考增量（reasoning_content，若存在）。
        if let Some(thinking) = delta.get("reasoning_content").and_then(|c| c.as_str())
            && !thinking.is_empty()
        {
            acc.append_thinking(thinking);
            events.push(AssistantEvent::ThinkingDelta {
                thinking: thinking.to_string(),
            });
        }

        // 工具调用（首块带 id/name，续块带 index + arguments）。
        if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
            for tc in tool_calls {
                let id = tc
                    .get("id")
                    .and_then(|i| i.as_str())
                    .unwrap_or_default()
                    .to_string();
                let index = tc.get("index").and_then(|i| i.as_u64()).map(|i| i as usize);
                let function = &tc["function"];
                let name = function
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default()
                    .to_string();
                let args = function
                    .get("arguments")
                    .and_then(|a| a.as_str())
                    .unwrap_or_default()
                    .to_string();

                let tc_idx = if !id.is_empty() {
                    // 新工具调用开始。
                    let idx = acc.start_tool_call(id.clone(), name.clone());
                    events.push(AssistantEvent::ToolCallStart {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: String::new(),
                    });
                    idx
                } else {
                    // 续块：按 index 定位。
                    match index {
                        Some(i) => i,
                        None => continue,
                    }
                };

                if !args.is_empty()
                    && let Some(tca) = acc.tool_calls.get_mut(tc_idx)
                {
                    tca.arguments.push_str(&args);
                    events.push(AssistantEvent::ToolCallDelta {
                        id: tca.id.clone(),
                        arguments_delta: args,
                    });
                }
            }
        }

        // finish_reason：记录 StopReason；tool_calls 时为每个未结束项发 ToolCallEnd。
        if let Some(finish_reason) = choice.get("finish_reason").and_then(|f| f.as_str()) {
            if finish_reason == "tool_calls" {
                for tc in acc.tool_calls.iter() {
                    if !tc.done {
                        events.push(AssistantEvent::ToolCallEnd { id: tc.id.clone() });
                    }
                }
                for tc in acc.tool_calls.iter_mut() {
                    tc.done = true;
                }
            }
            acc.stop_reason = Some(map_stop_reason(finish_reason));
        }
    }

    // usage（末 chunk，include_usage）。
    if let Some(usage) = v.get("usage")
        && !usage.is_null()
    {
        acc.usage = Some(map_usage(usage));
    }

    Ok(events)
}

/// OpenAI usage → 内部 [`Usage`]。
fn map_usage(v: &Value) -> Usage {
    let input = v.get("prompt_tokens").and_then(|t| t.as_u64()).unwrap_or(0);
    let output = v
        .get("completion_tokens")
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    let cache_read = v
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    let total = v
        .get("total_tokens")
        .and_then(|t| t.as_u64())
        .unwrap_or(input + output);
    Usage {
        input,
        output,
        cache_read,
        cache_write: 0,
        total_tokens: total,
        cost: 0.0,
    }
}

/// OpenAI finish_reason → 内部 [`StopReason`]。
fn map_stop_reason(s: &str) -> StopReason {
    match s {
        "stop" | "tool_calls" => StopReason::Completed,
        "length" => StopReason::Length,
        "content_filter" => StopReason::Error,
        other => StopReason::Other(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(data: &str) -> (Vec<AssistantEvent>, Acc) {
        let mut acc = Acc::new("m".into());
        let events = map_event(SseEvent::Data { data: data.into() }, &mut acc).expect("map chunk");
        (events, acc)
    }

    #[test]
    fn text_delta() {
        let (events, acc) = map(r#"{"choices":[{"delta":{"content":"Hello"}}]}"#);
        assert_eq!(
            events,
            vec![AssistantEvent::TextDelta {
                text: "Hello".into()
            }]
        );
        assert_eq!(acc.text, "Hello");
    }

    #[test]
    fn thinking_delta_from_reasoning_content() {
        let (events, acc) = map(r#"{"choices":[{"delta":{"reasoning_content":"hmm"}}]}"#);
        assert_eq!(
            events,
            vec![AssistantEvent::ThinkingDelta {
                thinking: "hmm".into()
            }]
        );
        assert_eq!(acc.thinking, "hmm");
    }

    #[test]
    fn tool_call_start() {
        let (events, acc) = map(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"search","arguments":""}}]}}]}"#,
        );
        assert_eq!(
            events,
            vec![AssistantEvent::ToolCallStart {
                id: "call_1".into(),
                name: "search".into(),
                arguments: String::new()
            }]
        );
        assert_eq!(acc.tool_calls.len(), 1);
        assert_eq!(acc.tool_calls[0].id, "call_1");
    }

    #[test]
    fn tool_call_delta_by_index() {
        let mut acc = Acc::new("m".into());
        // 先 start。
        let _ = map_event(
            SseEvent::Data {
                data: r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"search","arguments":""}}]}}]}"#.into(),
            },
            &mut acc,
        )
        .expect("start");
        // 再 delta（仅 index + arguments）。
        let events = map_event(
            SseEvent::Data {
                data: r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"q\":"}}]}}]}"#.into(),
            },
            &mut acc,
        )
        .expect("delta");
        assert_eq!(
            events,
            vec![AssistantEvent::ToolCallDelta {
                id: "call_1".into(),
                arguments_delta: "{\"q\":".into()
            }]
        );
        assert_eq!(acc.tool_calls[0].arguments, "{\"q\":");
    }

    #[test]
    fn finish_reason_tool_calls_emits_end() {
        let mut acc = Acc::new("m".into());
        let _ = map_event(
            SseEvent::Data {
                data: r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"search","arguments":"{}"}}]}}]}"#.into(),
            },
            &mut acc,
        )
        .expect("start");
        let events = map_event(
            SseEvent::Data {
                data: r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#.into(),
            },
            &mut acc,
        )
        .expect("finish");
        assert_eq!(
            events,
            vec![AssistantEvent::ToolCallEnd {
                id: "call_1".into()
            }]
        );
        assert_eq!(acc.stop_reason, Some(StopReason::Completed));
        assert!(acc.tool_calls[0].done);
    }

    #[test]
    fn finish_reason_stop_maps_completed() {
        let (_events, acc) = map(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#);
        assert_eq!(acc.stop_reason, Some(StopReason::Completed));
    }

    #[test]
    fn finish_reason_length_maps_length() {
        let (_events, acc) = map(r#"{"choices":[{"delta":{},"finish_reason":"length"}]}"#);
        assert_eq!(acc.stop_reason, Some(StopReason::Length));
    }

    #[test]
    fn usage_mapped() {
        let (_events, acc) = map(
            r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15,"prompt_tokens_details":{"cached_tokens":3}}}"#,
        );
        let usage = acc.usage.expect("usage set");
        assert_eq!(usage.input, 10);
        assert_eq!(usage.output, 5);
        assert_eq!(usage.cache_read, 3);
        assert_eq!(usage.cache_write, 0);
        assert_eq!(usage.total_tokens, 15);
    }

    #[test]
    fn done_event_builds_message() {
        let mut acc = Acc::new("gpt-4o".into());
        let _ = map_event(
            SseEvent::Data {
                data: r#"{"choices":[{"delta":{"content":"Hi"}}]}"#.into(),
            },
            &mut acc,
        )
        .expect("text");
        let events = map_event(SseEvent::Done, &mut acc).expect("done");
        assert_eq!(events.len(), 1);
        if let AssistantEvent::Done { message } = &events[0] {
            assert_eq!(
                message.model,
                Some(crate::core::message::ModelId("gpt-4o".into()))
            );
            assert_eq!(message.content.len(), 1);
        } else {
            panic!("expected Done");
        }
    }

    #[test]
    fn invalid_json_is_parse_error() {
        let result = map_event(
            SseEvent::Data {
                data: "not json".into(),
            },
            &mut Acc::new("m".into()),
        );
        assert!(matches!(result, Err(ProviderError::Parse(_))));
    }
}
