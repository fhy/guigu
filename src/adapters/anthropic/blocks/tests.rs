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

#[test]
fn duplicate_text_start_index_is_parse_error() {
    let mut acc = Acc::new("m".into());
    let _ = handle_block_start(
        &parse(r#"{"index":0,"content_block":{"type":"text","text":""}}"#),
        &mut acc,
    )
    .expect("start 0");
    let segs_before = acc.segments.len();
    let texts_before = acc.texts.len();
    // 同一 index 再次 start → Parse。
    let result = handle_block_start(
        &parse(r#"{"index":0,"content_block":{"type":"text","text":""}}"#),
        &mut acc,
    );
    assert!(matches!(result, Err(ProviderError::Parse(_))));
    // 状态未被污染：无新段、无新文本 block，原映射仍指向首个 block。
    assert_eq!(acc.segments.len(), segs_before);
    assert_eq!(acc.texts.len(), texts_before);
    assert!(matches!(acc.block_kind(0), Some(SegmentKind::Text(0))));
}

#[test]
fn duplicate_tool_use_start_index_is_parse_error() {
    let mut acc = Acc::new("m".into());
    let _ = handle_block_start(
        &parse(r#"{"index":0,"content_block":{"type":"tool_use","id":"tu_1","name":"search"}}"#),
        &mut acc,
    )
    .expect("start 0");
    let segs_before = acc.segments.len();
    let tcs_before = acc.tool_calls.len();
    // 同一 index 再次 start（不同 id）→ Parse。
    let result = handle_block_start(
        &parse(r#"{"index":0,"content_block":{"type":"tool_use","id":"tu_2","name":"other"}}"#),
        &mut acc,
    );
    assert!(matches!(result, Err(ProviderError::Parse(_))));
    // 状态未被污染：无新段、无新 tool call，原映射仍指向首个 tool call。
    assert_eq!(acc.segments.len(), segs_before);
    assert_eq!(acc.tool_calls.len(), tcs_before);
    assert!(matches!(acc.block_kind(0), Some(SegmentKind::ToolCall(0))));
    assert_eq!(acc.tool_calls[0].id, "tu_1");
}

#[test]
fn duplicate_thinking_start_index_is_parse_error() {
    let mut acc = Acc::new("m".into());
    let _ = handle_block_start(
        &parse(r#"{"index":0,"content_block":{"type":"thinking","thinking":""}}"#),
        &mut acc,
    )
    .expect("start 0");
    let segs_before = acc.segments.len();
    let thinkings_before = acc.thinkings.len();
    // 同一 index 再次 start → Parse。
    let result = handle_block_start(
        &parse(r#"{"index":0,"content_block":{"type":"thinking","thinking":""}}"#),
        &mut acc,
    );
    assert!(matches!(result, Err(ProviderError::Parse(_))));
    // 状态未被污染：无新段、无新 thinking block，原映射仍指向首个 block。
    assert_eq!(acc.segments.len(), segs_before);
    assert_eq!(acc.thinkings.len(), thinkings_before);
    assert!(matches!(acc.block_kind(0), Some(SegmentKind::Thinking(0))));
}

#[test]
fn duplicate_block_stop_tool_use_is_parse_error() {
    let mut acc = Acc::new("m".into());
    let _ = handle_block_start(
        &parse(r#"{"index":0,"content_block":{"type":"tool_use","id":"tu_1","name":"search"}}"#),
        &mut acc,
    )
    .expect("start 0");
    // 首次 stop → ToolCallEnd。
    let events = handle_block_stop(&parse(r#"{"index":0}"#), &mut acc).expect("stop 0");
    assert_eq!(
        events,
        vec![AssistantEvent::ToolCallEnd { id: "tu_1".into() }]
    );
    assert!(acc.tool_calls[0].done);
    // 同一 index 再次 stop → Parse，不重复发 ToolCallEnd。
    let result = handle_block_stop(&parse(r#"{"index":0}"#), &mut acc);
    assert!(matches!(result, Err(ProviderError::Parse(_))));
}
