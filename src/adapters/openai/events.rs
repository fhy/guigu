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
        // provider index 经显式映射定位本地工具调用（index 可能非连续）；
        // 未知 index / 续块先于 start / 重复 start 均返回 Parse，禁止静默忽略。
        if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
            for tc in tool_calls {
                let index = tc.get("index").and_then(|i| i.as_u64()).ok_or_else(|| {
                    ProviderError::Parse("tool_call chunk missing valid index".into())
                })? as usize;
                let id = tc
                    .get("id")
                    .and_then(|i| i.as_str())
                    .unwrap_or_default()
                    .to_string();
                let function = tc.get("function").ok_or_else(|| {
                    ProviderError::Parse("tool_call chunk missing function".into())
                })?;
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
                    // 新工具调用开始：登记 provider index → 本地 index 映射。
                    let idx = acc.start_tool_call(id.clone(), name.clone());
                    acc.map_tool_index(index, idx)?;
                    events.push(AssistantEvent::ToolCallStart {
                        id: id.clone(),
                        name,
                        arguments: String::new(),
                    });
                    idx
                } else {
                    // 续块：按显式映射定位；未知 index → Parse。
                    acc.tool_local_index(index).ok_or_else(|| {
                        ProviderError::Parse(format!(
                            "tool_call continuation with unknown provider index {index}"
                        ))
                    })?
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
mod tests;
