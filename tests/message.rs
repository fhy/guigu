use guigu::core::ToolResult;
use guigu::core::event::AgentEvent;
use guigu::core::message::{
    AssistantContent, AssistantMessage, ImageContent, Message, ModelId, StopReason, ThinkingLevel,
    ToolCall, ToolResultContent, ToolResultMessage, Usage, UserContent, UserMessage,
};
use serde_json;
use std::sync::Arc;

#[test]
fn test_user_message_roundtrip() {
    let original = Message::User(UserMessage {
        content: vec![
            UserContent::Text {
                text: "Hello".to_string(),
            },
            UserContent::Image(ImageContent {
                data: "data".to_string(),
                mime_type: "image/png".to_string(),
            }),
        ],
        timestamp: 1234567890,
    });

    let serialized = serde_json::to_string(&original).expect("Failed to serialize");
    let deserialized: Message = serde_json::from_str(&serialized).expect("Failed to deserialize");

    assert_eq!(original, deserialized);
}

#[test]
fn test_assistant_message_roundtrip() {
    let original = Message::Assistant(AssistantMessage {
        content: vec![
            AssistantContent::Text {
                text: "Hello".to_string(),
            },
            AssistantContent::Thinking {
                text: "Thinking...".to_string(),
            },
            AssistantContent::ToolCall(ToolCall {
                id: "call1".to_string(),
                name: "tool1".to_string(),
                arguments: r#"{"arg": "value"}"#.to_string(),
            }),
        ],
        model: Some(ModelId("gpt-4".to_string())),
        usage: Some(Usage {
            input: 100,
            output: 200,
            cache_read: 0,
            cache_write: 0,
            total_tokens: 300,
            cost: 0.01,
        }),
        stop_reason: Some(StopReason::Completed),
        error_message: None,
        timestamp: 1234567890,
    });

    let serialized = serde_json::to_string(&original).expect("Failed to serialize");
    let deserialized: Message = serde_json::from_str(&serialized).expect("Failed to deserialize");

    assert_eq!(original, deserialized);
}

#[test]
fn test_tool_result_message_roundtrip() {
    let original = Message::ToolResult(ToolResultMessage {
        tool_call_id: "call1".to_string(),
        tool_name: "tool1".to_string(),
        is_error: false,
        content: vec![
            ToolResultContent::Text {
                text: "Result".to_string(),
            },
            ToolResultContent::Image(ImageContent {
                data: "data".to_string(),
                mime_type: "image/png".to_string(),
            }),
        ],
        details: Some(serde_json::json!({"key": "value"})),
        timestamp: 1234567890,
    });

    let serialized = serde_json::to_string(&original).expect("Failed to serialize");
    let deserialized: Message = serde_json::from_str(&serialized).expect("Failed to deserialize");

    assert_eq!(original, deserialized);
}

#[test]
fn test_stop_reason_roundtrip() {
    let original = StopReason::Other("custom_reason".to_string());

    let serialized = serde_json::to_string(&original).expect("Failed to serialize");
    let deserialized: StopReason =
        serde_json::from_str(&serialized).expect("Failed to deserialize");

    assert_eq!(original, deserialized);
}

#[test]
fn test_stop_reason_unknown_variant() {
    // 测试未知的 stop reason 应该被兜底到 Other
    // 由于我们使用了内部标签格式，我们需要测试字符串形式的序列化
    let json = r#""some_future_reason""#;
    let deserialized: StopReason = serde_json::from_str(json).expect("Failed to deserialize");
    assert_eq!(
        deserialized,
        StopReason::Other("some_future_reason".to_string())
    );
}

#[test]
fn test_stop_reason_all_variants() {
    // 测试所有具名的 StopReason 变体
    let test_cases = vec![
        ("completed", StopReason::Completed),
        ("length", StopReason::Length),
        ("error", StopReason::Error),
        ("aborted", StopReason::Aborted),
        ("pending", StopReason::Pending),
    ];

    for (input, expected) in test_cases {
        // 由于我们使用了内部标签格式，需要测试字符串形式
        let json = format!(r#""{}""#, input);
        let deserialized: StopReason = serde_json::from_str(&json).expect("Failed to deserialize");
        assert_eq!(deserialized, expected, "Failed for input: {}", input);
    }
}

#[test]
fn test_agent_event_roundtrip() {
    // 测试所有 AgentEvent 变体
    let test_cases = vec![
        AgentEvent::AgentStart,
        AgentEvent::AgentEnd { messages: vec![] },
        AgentEvent::TurnStart,
        AgentEvent::TurnEnd {
            message: Arc::new(AssistantMessage {
                content: vec![],
                model: None,
                usage: None,
                stop_reason: None,
                error_message: None,
                timestamp: 0,
            }),
            tool_results: vec![],
        },
        AgentEvent::MessageStart {
            message: Arc::new(Message::User(UserMessage {
                content: vec![],
                timestamp: 0,
            })),
        },
        AgentEvent::MessageUpdate {
            message: Arc::new(Message::User(UserMessage {
                content: vec![],
                timestamp: 0,
            })),
            assistant_event: guigu::core::event::AssistantEvent,
        },
        AgentEvent::MessageEnd {
            message: Arc::new(Message::User(UserMessage {
                content: vec![],
                timestamp: 0,
            })),
        },
        AgentEvent::ToolExecutionStart {
            tool_call_id: "call1".to_string(),
            tool_name: "tool1".to_string(),
            args: serde_json::Value::Null,
        },
        AgentEvent::ToolExecutionUpdate {
            tool_call_id: "call1".to_string(),
            tool_name: "tool1".to_string(),
            args: serde_json::Value::Null,
            partial: ToolResult {
                content: vec![],
                is_error: false,
                details: None,
            },
        },
        AgentEvent::ToolExecutionEnd {
            tool_call_id: "call1".to_string(),
            tool_name: "tool1".to_string(),
            result: ToolResult {
                content: vec![],
                is_error: false,
                details: None,
            },
            is_error: false,
        },
    ];

    for (i, original) in test_cases.iter().enumerate() {
        let serialized = serde_json::to_string(original).expect("Failed to serialize");
        let deserialized: AgentEvent =
            serde_json::from_str(&serialized).expect("Failed to deserialize");
        assert_eq!(original, &deserialized, "Failed for test case {}", i);
    }
}

#[test]
fn test_thinking_level_roundtrip() {
    let original = ThinkingLevel::Medium;

    let serialized = serde_json::to_string(&original).expect("Failed to serialize");
    let deserialized: ThinkingLevel =
        serde_json::from_str(&serialized).expect("Failed to deserialize");

    assert_eq!(original, deserialized);
}

#[test]
fn test_tool_result_roundtrip() {
    let original = ToolResult {
        content: vec![
            ToolResultContent::Text {
                text: "Result".to_string(),
            },
            ToolResultContent::Image(ImageContent {
                data: "data".to_string(),
                mime_type: "image/png".to_string(),
            }),
        ],
        is_error: false,
        details: Some(serde_json::json!({"key": "value"})),
    };

    let serialized = serde_json::to_string(&original).expect("Failed to serialize");
    let deserialized: ToolResult =
        serde_json::from_str(&serialized).expect("Failed to deserialize");

    assert_eq!(original, deserialized);
}
