//! `AcpFsTool` 单测（Task 014）：fs 读写经 `AcpClient` 代理 + 权限判定。
//!
//! 从 `tests.rs` 拆出（单测试文件 ≤ 30 个 `#[test]` 约束）。共享工具见 `testutil`。

use std::sync::Arc;

use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::acp::{AcpClient, AcpFsTool, PermissionMode};
use crate::core::message::ToolResultContent;
use crate::core::tool::{Tool, ToolError};

use super::testutil::FakeClient;

/// `AcpFsTool` 读：经 client 代理（`fs/read_text_file`），bypassPermissions 不请求权限。
#[tokio::test]
async fn test_fs_tool_read() {
    let fake = Arc::new(
        FakeClient::new().with_response("fs/read_text_file", json!({ "content": "file content" })),
    );
    let client: Arc<dyn AcpClient> = fake.clone();
    let mode = Arc::new(tokio::sync::RwLock::new(PermissionMode::BypassPermissions));
    let tool = AcpFsTool::new(client, "s1".to_string(), mode);

    let result = tool
        .execute(
            "tc1",
            json!({ "operation": "read", "path": "/tmp/x" }),
            CancellationToken::new(),
            None,
        )
        .await
        .expect("read");
    assert!(!result.is_error);
    assert!(
        fake.has_call("fs/read_text_file"),
        "should call fs/read_text_file"
    );
    assert!(
        !fake.has_call("session/request_permission"),
        "bypass should not request permission"
    );
    // 结果含文件内容。
    match &result.content[0] {
        ToolResultContent::Text { text } => assert_eq!(text, "file content"),
        _ => panic!("expected text content"),
    }
}

/// `AcpFsTool` 写：经 client 代理（`fs/write_text_file`）。
#[tokio::test]
async fn test_fs_tool_write() {
    let fake = Arc::new(FakeClient::new());
    let client: Arc<dyn AcpClient> = fake.clone();
    let mode = Arc::new(tokio::sync::RwLock::new(PermissionMode::BypassPermissions));
    let tool = AcpFsTool::new(client, "s1".to_string(), mode);

    let result = tool
        .execute(
            "tc1",
            json!({ "operation": "write", "path": "/tmp/x", "content": "data" }),
            CancellationToken::new(),
            None,
        )
        .await
        .expect("write");
    assert!(!result.is_error);
    assert!(
        fake.has_call("fs/write_text_file"),
        "should call fs/write_text_file"
    );
    let write_params = fake.calls_with("fs/write_text_file");
    assert_eq!(write_params[0]["content"], "data");
    assert_eq!(write_params[0]["path"], "/tmp/x");
}

/// `AcpFsTool` 权限：mode=plan 时先发 `session/request_permission`，授权后才写。
#[tokio::test]
async fn test_fs_tool_permission_plan() {
    let fake = Arc::new(FakeClient::new().with_response(
        "session/request_permission",
        json!({ "outcome": { "outcome": "selected", "optionId": "allow_once" } }),
    ));
    let client: Arc<dyn AcpClient> = fake.clone();
    let mode = Arc::new(tokio::sync::RwLock::new(PermissionMode::Plan));
    let tool = AcpFsTool::new(client, "s1".to_string(), mode);

    let result = tool
        .execute(
            "tc1",
            json!({ "operation": "write", "path": "/tmp/x", "content": "data" }),
            CancellationToken::new(),
            None,
        )
        .await
        .expect("write");
    assert!(!result.is_error);
    // 先请求权限，再写文件。
    assert!(
        fake.has_call("session/request_permission"),
        "should request permission"
    );
    assert!(
        fake.has_call("fs/write_text_file"),
        "should write after permission"
    );
    let calls = fake.calls.lock().unwrap();
    let perm_idx = calls
        .iter()
        .position(|(m, _)| m == "session/request_permission")
        .expect("perm call");
    let write_idx = calls
        .iter()
        .position(|(m, _)| m == "fs/write_text_file")
        .expect("write call");
    assert!(perm_idx < write_idx, "permission should come before write");
}

/// `AcpFsTool` 权限：mode=plan 且 client 拒绝 → 不写文件，返回错误结果。
#[tokio::test]
async fn test_fs_tool_permission_denied() {
    let fake = Arc::new(FakeClient::new().with_response(
        "session/request_permission",
        json!({ "outcome": { "outcome": "cancelled" } }),
    ));
    let client: Arc<dyn AcpClient> = fake.clone();
    let mode = Arc::new(tokio::sync::RwLock::new(PermissionMode::Plan));
    let tool = AcpFsTool::new(client, "s1".to_string(), mode);

    let result = tool
        .execute(
            "tc1",
            json!({ "operation": "write", "path": "/tmp/x", "content": "data" }),
            CancellationToken::new(),
            None,
        )
        .await
        .expect("write");
    assert!(result.is_error, "should be error when denied");
    assert!(
        !fake.has_call("fs/write_text_file"),
        "should not write when denied"
    );
}

/// `AcpFsTool` 未知操作 → `ToolError`。
#[tokio::test]
async fn test_fs_tool_unknown_operation() {
    let fake = Arc::new(FakeClient::new());
    let client: Arc<dyn AcpClient> = fake.clone();
    let mode = Arc::new(tokio::sync::RwLock::new(PermissionMode::BypassPermissions));
    let tool = AcpFsTool::new(client, "s1".to_string(), mode);

    let result = tool
        .execute(
            "tc1",
            json!({ "operation": "delete", "path": "/tmp/x" }),
            CancellationToken::new(),
            None,
        )
        .await;
    match result {
        Err(ToolError { message }) => assert!(message.contains("unknown operation")),
        _ => panic!("expected ToolError for unknown operation"),
    }
}
