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
    assert_eq!(acc.texts, vec!["Hello".to_string()]);
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
    assert_eq!(acc.thinkings, vec!["hmm".to_string()]);
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
fn tool_call_non_contiguous_indices() {
    let mut acc = Acc::new("m".into());
    // 首块使用非连续 provider index（1 与 3）。
    let _ = map_event(
        SseEvent::Data {
            data: r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_a","function":{"name":"a","arguments":""}}]}}]}"#.into(),
        },
        &mut acc,
    )
    .expect("start a");
    let _ = map_event(
        SseEvent::Data {
            data: r#"{"choices":[{"delta":{"tool_calls":[{"index":3,"id":"call_b","function":{"name":"b","arguments":""}}]}}]}"#.into(),
        },
        &mut acc,
    )
    .expect("start b");
    // 交错续块：参数必须按 provider index 归位。
    let _ = map_event(
        SseEvent::Data {
            data: r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"x\":"}}]}}]}"#.into(),
        },
        &mut acc,
    )
    .expect("delta a");
    let _ = map_event(
        SseEvent::Data {
            data: r#"{"choices":[{"delta":{"tool_calls":[{"index":3,"function":{"arguments":"{\"y\":"}}]}}]}"#.into(),
        },
        &mut acc,
    )
    .expect("delta b");
    let _ = map_event(
        SseEvent::Data {
            data: r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"1}"}}]}}]}"#.into(),
        },
        &mut acc,
    )
    .expect("delta a2");
    let _ = map_event(
        SseEvent::Data {
            data: r#"{"choices":[{"delta":{"tool_calls":[{"index":3,"function":{"arguments":"2}"}}]}}]}"#.into(),
        },
        &mut acc,
    )
    .expect("delta b2");
    assert_eq!(acc.tool_calls.len(), 2);
    assert_eq!(acc.tool_calls[0].id, "call_a");
    assert_eq!(acc.tool_calls[0].arguments, "{\"x\":1}");
    assert_eq!(acc.tool_calls[1].id, "call_b");
    assert_eq!(acc.tool_calls[1].arguments, "{\"y\":2}");
}

#[test]
fn tool_call_unknown_index_is_parse_error() {
    let mut acc = Acc::new("m".into());
    let _ = map_event(
        SseEvent::Data {
            data: r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"t","arguments":""}}]}}]}"#.into(),
        },
        &mut acc,
    )
    .expect("start");
    // 续块 index 未登记（含续块先于 start 的情形）→ Parse，禁止静默丢弃。
    let result = map_event(
        SseEvent::Data {
            data: r#"{"choices":[{"delta":{"tool_calls":[{"index":5,"function":{"arguments":"x"}}]}}]}"#.into(),
        },
        &mut acc,
    );
    assert!(matches!(result, Err(ProviderError::Parse(_))));
}

#[test]
fn tool_call_duplicate_index_is_parse_error() {
    let mut acc = Acc::new("m".into());
    let _ = map_event(
        SseEvent::Data {
            data: r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"t","arguments":""}}]}}]}"#.into(),
        },
        &mut acc,
    )
    .expect("start 1");
    // 同一 provider index 再次 start → Parse。
    let result = map_event(
        SseEvent::Data {
            data: r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_2","function":{"name":"u","arguments":""}}]}}]}"#.into(),
        },
        &mut acc,
    );
    assert!(matches!(result, Err(ProviderError::Parse(_))));
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
