//! Anthropic `content_block_*` 事件处理（纯函数）。
//!
//! `content_block_start` / `content_block_delta` / `content_block_stop`
//! 由 [`super::events::dispatch`] 解析 JSON 后分发至此。

use serde_json::Value;

use crate::core::provider::AssistantEvent;

use super::super::acc::{Acc, SegmentKind};

/// `content_block_start`：tool_use → `ToolCallStart`；text/thinking 仅记录块种类。
pub(crate) fn handle_block_start(v: &Value, acc: &mut Acc) -> Vec<AssistantEvent> {
    let index = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
    let block = &v["content_block"];
    let block_type = block
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or_default();
    match block_type {
        "tool_use" => {
            let id = block
                .get("id")
                .and_then(|i| i.as_str())
                .unwrap_or_default()
                .to_string();
            let name = block
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or_default()
                .to_string();
            let tc_idx = acc.start_tool_call(id.clone(), name.clone());
            acc.note_block(index, SegmentKind::ToolCall(tc_idx));
            vec![AssistantEvent::ToolCallStart {
                id,
                name,
                arguments: String::new(),
            }]
        }
        "text" => {
            acc.ensure_text();
            acc.note_block(index, SegmentKind::Text);
            Vec::new()
        }
        "thinking" => {
            acc.ensure_thinking();
            acc.note_block(index, SegmentKind::Thinking);
            Vec::new()
        }
        _ => Vec::new(),
    }
}

/// `content_block_delta`：text_delta / thinking_delta / input_json_delta。
pub(crate) fn handle_block_delta(v: &Value, acc: &mut Acc) -> Vec<AssistantEvent> {
    let index = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
    let delta = &v["delta"];
    let delta_type = delta
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or_default();
    match delta_type {
        "text_delta" => {
            let text = delta
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string();
            acc.append_text(&text);
            vec![AssistantEvent::TextDelta { text }]
        }
        "thinking_delta" => {
            let thinking = delta
                .get("thinking")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string();
            acc.append_thinking(&thinking);
            vec![AssistantEvent::ThinkingDelta { thinking }]
        }
        "input_json_delta" => {
            let partial = delta
                .get("partial_json")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string();
            let tc_idx = match acc.block_kind(index) {
                Some(SegmentKind::ToolCall(i)) => *i,
                _ => return Vec::new(),
            };
            if let Some(tca) = acc.tool_calls.get_mut(tc_idx) {
                tca.arguments.push_str(&partial);
                vec![AssistantEvent::ToolCallDelta {
                    id: tca.id.clone(),
                    arguments_delta: partial,
                }]
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

/// `content_block_stop`：tool_use → `ToolCallEnd`；其它块不产出事件。
pub(crate) fn handle_block_stop(v: &Value, acc: &mut Acc) -> Vec<AssistantEvent> {
    let index = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
    let tc_idx = match acc.block_kind(index) {
        Some(SegmentKind::ToolCall(i)) => *i,
        _ => return Vec::new(),
    };
    let id = match acc.tool_calls.get(tc_idx) {
        Some(tca) => tca.id.clone(),
        None => return Vec::new(),
    };
    acc.end_tool_call(&id);
    vec![AssistantEvent::ToolCallEnd { id }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Value {
        serde_json::from_str(s).expect("valid json")
    }

    #[test]
    fn block_start_text_no_event() {
        let mut acc = Acc::new("m".into());
        let events = handle_block_start(
            &parse(r#"{"index":0,"content_block":{"type":"text","text":""}}"#),
            &mut acc,
        );
        assert!(events.is_empty());
        assert!(matches!(acc.block_kind(0), Some(SegmentKind::Text)));
    }

    #[test]
    fn block_start_tool_use_emits_start() {
        let mut acc = Acc::new("m".into());
        let events = handle_block_start(
            &parse(
                r#"{"index":0,"content_block":{"type":"tool_use","id":"tu_1","name":"search"}}"#,
            ),
            &mut acc,
        );
        assert_eq!(
            events,
            vec![AssistantEvent::ToolCallStart {
                id: "tu_1".into(),
                name: "search".into(),
                arguments: String::new()
            }]
        );
        assert!(matches!(acc.block_kind(0), Some(SegmentKind::ToolCall(0))));
    }

    #[test]
    fn text_delta() {
        let mut acc = Acc::new("m".into());
        let events = handle_block_delta(
            &parse(r#"{"index":0,"delta":{"type":"text_delta","text":"Hello"}}"#),
            &mut acc,
        );
        assert_eq!(
            events,
            vec![AssistantEvent::TextDelta {
                text: "Hello".into()
            }]
        );
        assert_eq!(acc.text, "Hello");
    }

    #[test]
    fn thinking_delta() {
        let mut acc = Acc::new("m".into());
        let events = handle_block_delta(
            &parse(r#"{"index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}"#),
            &mut acc,
        );
        assert_eq!(
            events,
            vec![AssistantEvent::ThinkingDelta {
                thinking: "hmm".into()
            }]
        );
        assert_eq!(acc.thinking, "hmm");
    }

    #[test]
    fn input_json_delta_accumulates() {
        let mut acc = Acc::new("m".into());
        let _ = handle_block_start(
            &parse(
                r#"{"index":0,"content_block":{"type":"tool_use","id":"tu_1","name":"search"}}"#,
            ),
            &mut acc,
        );
        let events = handle_block_delta(
            &parse(r#"{"index":0,"delta":{"type":"input_json_delta","partial_json":"{\"q\":"}}"#),
            &mut acc,
        );
        assert_eq!(
            events,
            vec![AssistantEvent::ToolCallDelta {
                id: "tu_1".into(),
                arguments_delta: "{\"q\":".into()
            }]
        );
        assert_eq!(acc.tool_calls[0].arguments, "{\"q\":");
    }

    #[test]
    fn block_stop_tool_use_emits_end() {
        let mut acc = Acc::new("m".into());
        let _ = handle_block_start(
            &parse(
                r#"{"index":0,"content_block":{"type":"tool_use","id":"tu_1","name":"search"}}"#,
            ),
            &mut acc,
        );
        let events = handle_block_stop(&parse(r#"{"index":0}"#), &mut acc);
        assert_eq!(
            events,
            vec![AssistantEvent::ToolCallEnd { id: "tu_1".into() }]
        );
        assert!(acc.tool_calls[0].done);
    }

    #[test]
    fn block_stop_text_no_event() {
        let mut acc = Acc::new("m".into());
        let _ = handle_block_start(
            &parse(r#"{"index":0,"content_block":{"type":"text","text":""}}"#),
            &mut acc,
        );
        let events = handle_block_stop(&parse(r#"{"index":0}"#), &mut acc);
        assert!(events.is_empty());
    }
}
