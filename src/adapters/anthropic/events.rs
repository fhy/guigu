//! Anthropic 事件映射（纯函数：SSE `event`+`data` → `AssistantEvent` + 累积）。
//!
//! `event:` 区分类型（`content_block_*` / `message_*`），`data:` 为 JSON。
//! `content_block_*` 处理见 [`super::blocks`]。

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
    let (event_type, data) = match sse {
        SseEvent::Named { event, data } => (event, data),
        SseEvent::Data { data } => {
            // 无 event 名：从 JSON `type` 字段推断。
            let v: Value =
                serde_json::from_str(&data).map_err(|e| ProviderError::Parse(e.to_string()))?;
            let t = v
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string();
            (t, data)
        }
        SseEvent::Done => {
            return Ok(vec![AssistantEvent::Done {
                message: acc.build_message(),
            }]);
        }
    };
    dispatch(&event_type, &data, acc)
}

/// 按事件类型分发映射。
fn dispatch(
    event_type: &str,
    data: &str,
    acc: &mut Acc,
) -> Result<Vec<AssistantEvent>, ProviderError> {
    let v: Value = serde_json::from_str(data).map_err(|e| ProviderError::Parse(e.to_string()))?;
    match event_type {
        "content_block_start" => Ok(super::blocks::handle_block_start(&v, acc)),
        "content_block_delta" => Ok(super::blocks::handle_block_delta(&v, acc)),
        "content_block_stop" => Ok(super::blocks::handle_block_stop(&v, acc)),
        "message_start" => {
            // input_tokens 在 message_start 提供。
            if let Some(input) = v
                .get("message")
                .and_then(|m| m.get("usage"))
                .and_then(|u| u.get("input_tokens"))
                .and_then(|t| t.as_u64())
            {
                acc.input_tokens = input;
            }
            Ok(vec![])
        }
        "message_delta" => {
            let delta = &v["delta"];
            if let Some(sr) = delta.get("stop_reason").and_then(|s| s.as_str()) {
                acc.stop_reason = Some(map_stop_reason(sr));
            }
            if let Some(usage) = v.get("usage")
                && !usage.is_null()
            {
                acc.usage = Some(map_usage(usage, acc.input_tokens));
            }
            Ok(vec![])
        }
        "message_stop" => Ok(vec![AssistantEvent::Done {
            message: acc.build_message(),
        }]),
        _ => Ok(vec![]),
    }
}

/// Anthropic usage → 内部 [`Usage`]。
///
/// `input_tokens` 优先取 `message_start` 累积值（`acc_input`），否则取本对象。
fn map_usage(v: &Value, acc_input: u64) -> Usage {
    let input = if acc_input > 0 {
        acc_input
    } else {
        v.get("input_tokens").and_then(|t| t.as_u64()).unwrap_or(0)
    };
    let output = v.get("output_tokens").and_then(|t| t.as_u64()).unwrap_or(0);
    let cache_read = v
        .get("cache_read_input_tokens")
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    let cache_write = v
        .get("cache_creation_input_tokens")
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    Usage {
        input,
        output,
        cache_read,
        cache_write,
        total_tokens: input + output,
        cost: 0.0,
    }
}

/// Anthropic stop_reason → 内部 [`StopReason`]。
fn map_stop_reason(s: &str) -> StopReason {
    match s {
        "end_turn" | "stop_sequence" | "tool_use" => StopReason::Completed,
        "max_tokens" => StopReason::Length,
        "refusal" => StopReason::Error,
        other => StopReason::Other(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(event: &str, data: &str) -> SseEvent {
        SseEvent::Named {
            event: event.into(),
            data: data.into(),
        }
    }

    fn map(event: &str, data: &str, acc: &mut Acc) -> Vec<AssistantEvent> {
        map_event(named(event, data), acc).expect("map event")
    }

    #[test]
    fn message_start_records_input_tokens() {
        let mut acc = Acc::new("m".into());
        let events = map(
            "message_start",
            r#"{"type":"message_start","message":{"usage":{"input_tokens":5,"output_tokens":1}}}"#,
            &mut acc,
        );
        assert!(events.is_empty());
        assert_eq!(acc.input_tokens, 5);
    }

    #[test]
    fn message_delta_records_stop_reason_and_usage() {
        let mut acc = Acc::new("m".into());
        let events = map(
            "message_delta",
            r#"{"delta":{"stop_reason":"end_turn"},"usage":{"input_tokens":7,"output_tokens":3}}"#,
            &mut acc,
        );
        assert!(events.is_empty());
        assert_eq!(acc.stop_reason, Some(StopReason::Completed));
        let usage = acc.usage.expect("usage");
        assert_eq!(usage.input, 7);
        assert_eq!(usage.output, 3);
        assert_eq!(usage.total_tokens, 10);
    }

    #[test]
    fn message_delta_usage_prefers_message_start_input() {
        let mut acc = Acc::new("m".into());
        let _ = map(
            "message_start",
            r#"{"message":{"usage":{"input_tokens":5}}}"#,
            &mut acc,
        );
        let _ = map(
            "message_delta",
            r#"{"delta":{},"usage":{"output_tokens":2}}"#,
            &mut acc,
        );
        let usage = acc.usage.expect("usage");
        assert_eq!(usage.input, 5);
        assert_eq!(usage.output, 2);
        assert_eq!(usage.total_tokens, 7);
    }

    #[test]
    fn message_stop_builds_done() {
        let mut acc = Acc::new("claude".into());
        let _ = map(
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"text_delta","text":"Hi"}}"#,
            &mut acc,
        );
        let events = map("message_stop", r#"{}"#, &mut acc);
        assert_eq!(events.len(), 1);
        if let AssistantEvent::Done { message } = &events[0] {
            assert_eq!(
                message.model,
                Some(crate::core::message::ModelId("claude".into()))
            );
            assert_eq!(message.content.len(), 1);
        } else {
            panic!("expected Done");
        }
    }

    #[test]
    fn stop_reason_max_tokens_maps_length() {
        let mut acc = Acc::new("m".into());
        let _ = map(
            "message_delta",
            r#"{"delta":{"stop_reason":"max_tokens"}}"#,
            &mut acc,
        );
        assert_eq!(acc.stop_reason, Some(StopReason::Length));
    }

    #[test]
    fn usage_cache_fields() {
        let mut acc = Acc::new("m".into());
        let _ = map(
            "message_delta",
            r#"{"delta":{},"usage":{"input_tokens":5,"output_tokens":2,"cache_read_input_tokens":1,"cache_creation_input_tokens":4}}"#,
            &mut acc,
        );
        let usage = acc.usage.expect("usage");
        assert_eq!(usage.cache_read, 1);
        assert_eq!(usage.cache_write, 4);
    }

    #[test]
    fn invalid_json_is_parse_error() {
        let mut acc = Acc::new("m".into());
        let result = map_event(named("content_block_delta", "not json"), &mut acc);
        assert!(matches!(result, Err(ProviderError::Parse(_))));
    }
}
