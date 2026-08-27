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
    )
    .expect("start");
    assert!(events.is_empty());
    assert!(matches!(acc.block_kind(0), Some(SegmentKind::Text(0))));
}

#[test]
fn block_start_tool_use_emits_start() {
    let mut acc = Acc::new("m".into());
    let events = handle_block_start(
        &parse(r#"{"index":0,"content_block":{"type":"tool_use","id":"tu_1","name":"search"}}"#),
        &mut acc,
    )
    .expect("start");
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
    let _ = handle_block_start(
        &parse(r#"{"index":0,"content_block":{"type":"text","text":""}}"#),
        &mut acc,
    )
    .expect("start");
    let events = handle_block_delta(
        &parse(r#"{"index":0,"delta":{"type":"text_delta","text":"Hello"}}"#),
        &mut acc,
    )
    .expect("delta");
    assert_eq!(
        events,
        vec![AssistantEvent::TextDelta {
            text: "Hello".into()
        }]
    );
    assert_eq!(acc.texts, vec!["Hello".to_string()]);
}

#[test]
fn thinking_delta() {
    let mut acc = Acc::new("m".into());
    let _ = handle_block_start(
        &parse(r#"{"index":0,"content_block":{"type":"thinking","thinking":""}}"#),
        &mut acc,
    )
    .expect("start");
    let events = handle_block_delta(
        &parse(r#"{"index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}"#),
        &mut acc,
    )
    .expect("delta");
    assert_eq!(
        events,
        vec![AssistantEvent::ThinkingDelta {
            thinking: "hmm".into()
        }]
    );
    assert_eq!(acc.thinkings, vec!["hmm".to_string()]);
}

#[test]
fn input_json_delta_accumulates() {
    let mut acc = Acc::new("m".into());
    let _ = handle_block_start(
        &parse(r#"{"index":0,"content_block":{"type":"tool_use","id":"tu_1","name":"search"}}"#),
        &mut acc,
    )
    .expect("start");
    let events = handle_block_delta(
        &parse(r#"{"index":0,"delta":{"type":"input_json_delta","partial_json":"{\"q\":"}}"#),
        &mut acc,
    )
    .expect("delta");
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
        &parse(r#"{"index":0,"content_block":{"type":"tool_use","id":"tu_1","name":"search"}}"#),
        &mut acc,
    )
    .expect("start");
    let events = handle_block_stop(&parse(r#"{"index":0}"#), &mut acc).expect("stop");
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
    )
    .expect("start");
    let events = handle_block_stop(&parse(r#"{"index":0}"#), &mut acc).expect("stop");
    assert!(events.is_empty());
}

#[test]
fn interleaved_blocks_keep_independent_content() {
    // text(0) → tool_use(1) → text(2)：两个文本块独立累积，顺序保留。
    let mut acc = Acc::new("m".into());
    let _ = handle_block_start(
        &parse(r#"{"index":0,"content_block":{"type":"text","text":""}}"#),
        &mut acc,
    )
    .expect("start 0");
    let _ = handle_block_delta(
        &parse(r#"{"index":0,"delta":{"type":"text_delta","text":"A"}}"#),
        &mut acc,
    )
    .expect("delta 0");
    let _ = handle_block_start(
        &parse(r#"{"index":1,"content_block":{"type":"tool_use","id":"tu_1","name":"search"}}"#),
        &mut acc,
    )
    .expect("start 1");
    let _ = handle_block_delta(
        &parse(r#"{"index":1,"delta":{"type":"input_json_delta","partial_json":"{\"q\":1}"}}"#),
        &mut acc,
    )
    .expect("delta 1");
    let _ = handle_block_start(
        &parse(r#"{"index":2,"content_block":{"type":"text","text":""}}"#),
        &mut acc,
    )
    .expect("start 2");
    let _ = handle_block_delta(
        &parse(r#"{"index":2,"delta":{"type":"text_delta","text":"B"}}"#),
        &mut acc,
    )
    .expect("delta 2");
    let events = handle_block_stop(&parse(r#"{"index":1}"#), &mut acc).expect("stop 1");
    assert_eq!(
        events,
        vec![AssistantEvent::ToolCallEnd { id: "tu_1".into() }]
    );
    assert_eq!(acc.texts, vec!["A".to_string(), "B".to_string()]);
    assert_eq!(acc.tool_calls[0].arguments, "{\"q\":1}");
    let msg = acc.build_message();
    assert_eq!(msg.content.len(), 3);
    assert!(
        matches!(&msg.content[0], crate::core::message::AssistantContent::Text { text } if text == "A")
    );
    assert!(
        matches!(&msg.content[2], crate::core::message::AssistantContent::Text { text } if text == "B")
    );
}

#[test]
fn missing_index_is_parse_error() {
    let mut acc = Acc::new("m".into());
    let result = handle_block_start(
        &parse(r#"{"content_block":{"type":"text","text":""}}"#),
        &mut acc,
    );
    assert!(matches!(result, Err(ProviderError::Parse(_))));
}

#[test]
fn unknown_block_type_is_parse_error() {
    let mut acc = Acc::new("m".into());
    let result = handle_block_start(
        &parse(r#"{"index":0,"content_block":{"type":"mystery"}}"#),
        &mut acc,
    );
    assert!(matches!(result, Err(ProviderError::Parse(_))));
}

#[test]
fn delta_before_start_is_parse_error() {
    let mut acc = Acc::new("m".into());
    let result = handle_block_delta(
        &parse(r#"{"index":0,"delta":{"type":"text_delta","text":"A"}}"#),
        &mut acc,
    );
    assert!(matches!(result, Err(ProviderError::Parse(_))));
}

#[test]
fn stop_unknown_block_is_parse_error() {
    let mut acc = Acc::new("m".into());
    let result = handle_block_stop(&parse(r#"{"index":5}"#), &mut acc);
    assert!(matches!(result, Err(ProviderError::Parse(_))));
}
