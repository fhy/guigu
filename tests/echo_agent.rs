//! EchoTool 集成测试：验证 echo 工具的真实调用逻辑。

use std::collections::HashMap;

use guigu::core::message::ToolResultContent;
use guigu::core::tool::{ResourceScope, Tool};
use guigu::tools::EchoTool;

/// EchoTool 应返回名称 "echo"。
#[test]
fn test_echo_tool_name() {
    let tool = EchoTool;
    assert_eq!(tool.name(), "echo");
}

/// EchoTool 应为只读资源范围。
#[test]
fn test_echo_tool_resource_scope() {
    let tool = EchoTool;
    assert_eq!(tool.resource_scope(), ResourceScope::ReadOnly);
}

/// EchoTool.call 应回显输入消息。
#[test]
fn test_echo_tool_call_echoes_message() {
    let tool = EchoTool;
    let args = serde_json::json!({ "message": "hello world" });
    let context = HashMap::new();

    let result = tool.call(args, &context).expect("echo call should succeed");

    assert!(!result.is_error, "echo should not be an error");
    assert_eq!(result.content.len(), 1, "echo should return 1 content item");
    match &result.content[0] {
        ToolResultContent::Text { text } => {
            assert_eq!(text, "hello world", "echo should return the input message");
        }
        other => panic!("expected Text content, got {:?}", other),
    }
}

/// EchoTool.call 对空消息应回显空字符串。
#[test]
fn test_echo_tool_call_empty_message() {
    let tool = EchoTool;
    let args = serde_json::json!({ "message": "" });
    let context = HashMap::new();

    let result = tool.call(args, &context).expect("echo call should succeed");

    assert!(!result.is_error);
    match &result.content[0] {
        ToolResultContent::Text { text } => {
            assert_eq!(text, "", "echo should return empty string for empty input");
        }
        other => panic!("expected Text content, got {:?}", other),
    }
}

/// EchoTool.call 对缺少 message 字段应返回错误。
#[test]
fn test_echo_tool_call_missing_message() {
    let tool = EchoTool;
    let args = serde_json::json!({});
    let context = HashMap::new();

    let result = tool.call(args, &context);
    assert!(
        result.is_err(),
        "echo should fail when message field is missing"
    );
}
