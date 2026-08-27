use super::*;
use crate::core::message::AssistantContent;

#[test]
fn text_accumulation() {
    let mut acc = Acc::new("m".into());
    acc.append_text("Hello, ");
    acc.append_text("world");
    assert_eq!(acc.texts, vec!["Hello, world".to_string()]);
    let msg = acc.build_message();
    assert_eq!(
        msg.content,
        vec![AssistantContent::Text {
            text: "Hello, world".into()
        }]
    );
}

#[test]
fn thinking_before_text_order() {
    let mut acc = Acc::new("m".into());
    acc.append_thinking("hmm");
    acc.append_text("answer");
    let msg = acc.build_message();
    assert_eq!(
        msg.content,
        vec![
            AssistantContent::Thinking { text: "hmm".into() },
            AssistantContent::Text {
                text: "answer".into()
            },
        ]
    );
}

#[test]
fn text_before_tool_call_order() {
    let mut acc = Acc::new("m".into());
    acc.append_text("let me use a tool");
    acc.start_tool_call("id1".into(), "search".into());
    acc.tool_calls[0].arguments.push_str("{\"q\":");
    acc.tool_calls[0].arguments.push_str("\"rust\"}");
    let msg = acc.build_message();
    assert_eq!(
        msg.content,
        vec![
            AssistantContent::Text {
                text: "let me use a tool".into()
            },
            AssistantContent::ToolCall(ToolCall {
                id: "id1".into(),
                name: "search".into(),
                arguments: "{\"q\":\"rust\"}".into(),
            }),
        ]
    );
}

#[test]
fn multiple_tool_calls_arguments_accumulate() {
    let mut acc = Acc::new("m".into());
    acc.start_tool_call("a".into(), "tool_a".into());
    acc.start_tool_call("b".into(), "tool_b".into());
    // 按 index 累积（与生产代码一致）。
    acc.tool_calls[1].arguments.push_str("{\"x\":1}");
    acc.tool_calls[0].arguments.push_str("{\"y\":2}");
    acc.tool_calls[1].arguments.push_str("");
    let msg = acc.build_message();
    let calls: Vec<ToolCall> = msg
        .content
        .iter()
        .filter_map(|c| match c {
            AssistantContent::ToolCall(tc) => Some(tc.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].id, "a");
    assert_eq!(calls[0].arguments, "{\"y\":2}");
    assert_eq!(calls[1].id, "b");
    assert_eq!(calls[1].arguments, "{\"x\":1}");
}

#[test]
fn end_tool_call_marks_done() {
    let mut acc = Acc::new("m".into());
    let idx = acc.start_tool_call("id1".into(), "t".into());
    assert!(!acc.tool_calls[idx].done);
    acc.end_tool_call("id1");
    assert!(acc.tool_calls[idx].done);
}

#[test]
fn interleaved_text_tool_text_blocks_stay_separate() {
    // Anthropic：text(0) → tool_use(1) → text(2)，两个文本块独立累积。
    let mut acc = Acc::new("m".into());
    let t0 = acc.start_text_block();
    acc.append_text_block(t0, "Let me check.");
    let tc = acc.start_tool_call("tu_1".into(), "search".into());
    acc.tool_calls[tc].arguments.push_str("{\"q\":\"rust\"}");
    let t1 = acc.start_text_block();
    acc.append_text_block(t1, "Done.");
    let msg = acc.build_message();
    assert_eq!(
        msg.content,
        vec![
            AssistantContent::Text {
                text: "Let me check.".into()
            },
            AssistantContent::ToolCall(ToolCall {
                id: "tu_1".into(),
                name: "search".into(),
                arguments: "{\"q\":\"rust\"}".into(),
            }),
            AssistantContent::Text {
                text: "Done.".into()
            },
        ]
    );
}

#[test]
fn multiple_thinking_blocks_stay_separate() {
    let mut acc = Acc::new("m".into());
    let th0 = acc.start_thinking_block();
    acc.append_thinking_block(th0, "first");
    let th1 = acc.start_thinking_block();
    acc.append_thinking_block(th1, "second");
    let msg = acc.build_message();
    assert_eq!(
        msg.content,
        vec![
            AssistantContent::Thinking {
                text: "first".into()
            },
            AssistantContent::Thinking {
                text: "second".into()
            },
        ]
    );
}

#[test]
fn block_kind_tracking() {
    let mut acc = Acc::new("m".into());
    let tc_idx = acc.start_tool_call("id1".into(), "t".into());
    acc.note_block(0, SegmentKind::Text(0));
    acc.note_block(1, SegmentKind::ToolCall(tc_idx));
    assert!(matches!(acc.block_kind(0), Some(SegmentKind::Text(0))));
    assert!(matches!(acc.block_kind(1), Some(SegmentKind::ToolCall(0))));
    assert!(acc.block_kind(5).is_none());
}

#[test]
fn tool_index_map_tracks_provider_indices() {
    let mut acc = Acc::new("m".into());
    let a = acc.start_tool_call("a".into(), "ta".into());
    acc.map_tool_index(1, a).expect("map index 1");
    let b = acc.start_tool_call("b".into(), "tb".into());
    acc.map_tool_index(3, b).expect("map index 3");
    assert_eq!(acc.tool_local_index(1), Some(a));
    assert_eq!(acc.tool_local_index(3), Some(b));
    assert_eq!(acc.tool_local_index(0), None);
    assert_eq!(acc.tool_local_index(9), None);
    // 同一 provider index 重复 start → Parse。
    assert!(matches!(
        acc.map_tool_index(1, b),
        Err(ProviderError::Parse(_))
    ));
}

#[test]
fn build_message_carries_usage_stop_reason_model() {
    let mut acc = Acc::new("gpt-x".into());
    acc.append_text("hi");
    acc.usage = Some(Usage {
        input: 10,
        output: 5,
        cache_read: 0,
        cache_write: 0,
        total_tokens: 15,
        cost: 0.0,
    });
    acc.stop_reason = Some(StopReason::Completed);
    let msg = acc.build_message();
    assert_eq!(msg.model, Some(ModelId("gpt-x".into())));
    assert_eq!(msg.stop_reason, Some(StopReason::Completed));
    assert!(msg.usage.is_some());
    assert_eq!(msg.usage.as_ref().unwrap().total_tokens, 15);
    assert!(msg.error_message.is_none());
}

#[test]
fn empty_acc_produces_empty_content() {
    let acc = Acc::new("m".into());
    let msg = acc.build_message();
    assert!(msg.content.is_empty());
    assert!(msg.usage.is_none());
    assert!(msg.stop_reason.is_none());
}
