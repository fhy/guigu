//! EchoTool 集成测试：验证 echo 工具的真实调用逻辑（新 async Tool trait）。

use guigu::core::message::ToolResultContent;
use guigu::core::tool::{ResourceScope, Tool};
use guigu::tools::EchoTool;
use tokio_util::sync::CancellationToken;

/// EchoTool 应返回名称 "echo"。
#[test]
fn test_echo_tool_name() {
    let tool = EchoTool;
    assert_eq!(tool.name(), "echo");
}

/// EchoTool 应有非空描述。
#[test]
fn test_echo_tool_description() {
    let tool = EchoTool;
    assert!(
        !tool.description().is_empty(),
        "description should not be empty"
    );
}

/// EchoTool 应声明参数 schema。
#[test]
fn test_echo_tool_parameters() {
    let tool = EchoTool;
    assert!(tool.parameters().is_some(), "parameters should be declared");
}

/// EchoTool 应为只读资源范围。
#[test]
fn test_echo_tool_resource_scope() {
    let tool = EchoTool;
    assert_eq!(tool.resource_scope(), ResourceScope::ReadOnly);
}

/// EchoTool.execute 应回显输入消息。
#[tokio::test]
async fn test_echo_tool_execute_echoes_message() {
    let tool = EchoTool;
    let args = serde_json::json!({ "message": "hello world" });
    let signal = CancellationToken::new();

    let result = tool
        .execute("call1", args, signal, None)
        .await
        .expect("echo execute should succeed");

    assert!(!result.is_error, "echo should not be an error");
    assert_eq!(result.content.len(), 1, "echo should return 1 content item");
    match &result.content[0] {
        ToolResultContent::Text { text } => {
            assert_eq!(text, "hello world", "echo should return the input message");
        }
        other => panic!("expected Text content, got {:?}", other),
    }
}

/// EchoTool.execute 对空消息应回显空字符串。
#[tokio::test]
async fn test_echo_tool_execute_empty_message() {
    let tool = EchoTool;
    let args = serde_json::json!({ "message": "" });
    let signal = CancellationToken::new();

    let result = tool
        .execute("call1", args, signal, None)
        .await
        .expect("echo execute should succeed");

    assert!(!result.is_error);
    match &result.content[0] {
        ToolResultContent::Text { text } => {
            assert_eq!(text, "", "echo should return empty string for empty input");
        }
        other => panic!("expected Text content, got {:?}", other),
    }
}

/// EchoTool.execute 对缺少 message 字段应返回错误。
#[tokio::test]
async fn test_echo_tool_execute_missing_message() {
    let tool = EchoTool;
    let args = serde_json::json!({});
    let signal = CancellationToken::new();

    let result = tool.execute("call1", args, signal, None).await;
    assert!(
        result.is_err(),
        "echo should fail when message field is missing"
    );
}
