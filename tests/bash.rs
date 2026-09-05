//! Task 006 集成测试：BashTool（`sh -c` 真实子进程 + 取消 kill + 超时）。
//!
//! 走完整 `Tool` trait 契约（name/description/parameters/resource_scope/execute）。
//! 命令一律用 `sh -c`（POSIX，不依赖 bash 二进制）。临时目录用
//! `std::env::temp_dir()` + 唯一后缀，不硬编码路径，测试结束清理。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use guigu::core::message::ToolResultContent;
use guigu::core::tool::{ResourceScope, Tool, ToolResult};
use guigu::tools::BashTool;
use tokio_util::sync::CancellationToken;

/// 取结果中唯一的 Text 内容。
fn text_of(result: &ToolResult) -> String {
    match &result.content[0] {
        ToolResultContent::Text { text } => text.clone(),
        other => panic!("expected Text content, got {other:?}"),
    }
}

/// 生成唯一临时目录（进程 id + 计数器），保证测试间隔离。
fn temp_dir_unique() -> std::path::PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!("guigu-bash-{}-{}", std::process::id(), n))
}

/// 构造无 default_cwd 的 BashTool（测试统一入口，保持旧行为）。
fn tool() -> BashTool {
    BashTool::new(None)
}

/// BashTool 名称应为 "bash"。
#[test]
fn test_bash_tool_name() {
    assert_eq!(tool().name(), "bash");
}

/// BashTool 应为 Exclusive 范围。
#[test]
fn test_bash_tool_resource_scope() {
    assert_eq!(tool().resource_scope(), ResourceScope::Exclusive);
}

/// BashTool 应声明参数 schema（command 必填）。
#[test]
fn test_bash_tool_parameters() {
    let params = tool().parameters().expect("parameters should be declared");
    assert_eq!(params["type"], "object");
    let required = params["required"]
        .as_array()
        .expect("required should be an array");
    assert!(required.contains(&serde_json::json!("command")));
}

/// `sh -c "echo hello"` 返回 stdout，details 含 exit_code=0。
#[tokio::test]
async fn test_bash_echo() {
    let result = tool()
        .execute(
            "c1",
            serde_json::json!({ "command": "echo hello" }),
            CancellationToken::new(),
            None,
        )
        .await
        .expect("bash should succeed");
    assert!(!result.is_error);
    assert_eq!(text_of(&result), "hello\n");
    let details = result.details.as_ref().expect("details should be present");
    assert_eq!(details["exit_code"], 0);
}

/// 非零退出返回 is_error=true 且 details 含 exit_code（不 throw）。
#[tokio::test]
async fn test_bash_nonzero_exit() {
    let result = tool()
        .execute(
            "c1",
            serde_json::json!({ "command": "echo oops 1>&2; exit 3" }),
            CancellationToken::new(),
            None,
        )
        .await
        .expect("non-zero exit should be Ok(is_error), not Err");
    assert!(result.is_error, "non-zero exit should set is_error");
    let details = result.details.as_ref().expect("details should be present");
    assert_eq!(details["exit_code"], 3);
    assert_eq!(details["stderr"].as_str(), Some("oops\n"));
}

/// timeout_ms 触发时 kill 子进程并返回含 "timeout" 错误。
#[tokio::test]
async fn test_bash_timeout() {
    let start = std::time::Instant::now();
    let result = tool()
        .execute(
            "c1",
            serde_json::json!({ "command": "sleep 5", "timeout_ms": 100 }),
            CancellationToken::new(),
            None,
        )
        .await;
    let elapsed = start.elapsed();
    match result {
        Err(e) => assert!(
            e.message.contains("timeout"),
            "should be a timeout error, got: {}",
            e.message
        ),
        Ok(_) => panic!("should time out"),
    }
    // 子进程被 kill，不应等满 5s。
    assert!(
        elapsed < Duration::from_secs(2),
        "should kill promptly, took {elapsed:?}"
    );
}

/// signal 取消时 kill 子进程并返回含 "cancelled" 错误。
#[tokio::test]
async fn test_bash_cancelled() {
    let signal = CancellationToken::new();
    let sig2 = signal.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        sig2.cancel();
    });
    let start = std::time::Instant::now();
    let result = tool()
        .execute(
            "c1",
            serde_json::json!({ "command": "sleep 5" }),
            signal,
            None,
        )
        .await;
    let elapsed = start.elapsed();
    match result {
        Err(e) => assert!(
            e.message.contains("cancelled"),
            "should be a cancelled error, got: {}",
            e.message
        ),
        Ok(_) => panic!("should be cancelled"),
    }
    assert!(
        elapsed < Duration::from_secs(2),
        "should kill promptly, took {elapsed:?}"
    );
}

/// 入口已取消时不 spawn，直接返回取消错误。
#[tokio::test]
async fn test_bash_pre_cancelled() {
    let signal = CancellationToken::new();
    signal.cancel();
    let result = tool()
        .execute(
            "c1",
            serde_json::json!({ "command": "echo never" }),
            signal,
            None,
        )
        .await;
    match result {
        Err(e) => assert!(
            e.message.contains("cancelled"),
            "should be cancelled, got: {}",
            e.message
        ),
        Ok(_) => panic!("should fail when pre-cancelled"),
    }
}

/// 缺少 command 字段应返回 invalid_arguments。
#[tokio::test]
async fn test_bash_missing_command() {
    let result = tool()
        .execute("c1", serde_json::json!({}), CancellationToken::new(), None)
        .await;
    match result {
        Err(e) => assert!(
            !e.message.contains("cancelled"),
            "should be invalid_arguments, got: {}",
            e.message
        ),
        Ok(_) => panic!("should fail when command is missing"),
    }
}

/// cwd 生效：`pwd` 输出等于规范化后的 cwd。
#[tokio::test]
async fn test_bash_cwd() {
    let dir = temp_dir_unique();
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let canonical = std::fs::canonicalize(&dir).expect("canonicalize dir");

    let result = tool()
        .execute(
            "c1",
            serde_json::json!({ "command": "pwd", "cwd": dir.to_string_lossy() }),
            CancellationToken::new(),
            None,
        )
        .await
        .expect("bash should succeed");
    assert!(!result.is_error);
    assert_eq!(text_of(&result).trim(), canonical.to_string_lossy());

    let _ = std::fs::remove_dir_all(&dir);
}

/// default_cwd 生效：per-call `cwd` 缺省时回退构造注入的 `default_cwd`（017-b）。
#[tokio::test]
async fn test_bash_default_cwd_fallback() {
    let dir = temp_dir_unique();
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let canonical = std::fs::canonicalize(&dir).expect("canonicalize dir");

    let tool = BashTool::new(Some(dir.clone()));
    let result = tool
        .execute(
            "c1",
            serde_json::json!({ "command": "pwd" }),
            CancellationToken::new(),
            None,
        )
        .await
        .expect("bash should succeed");
    assert!(!result.is_error);
    assert_eq!(
        text_of(&result).trim(),
        canonical.to_string_lossy(),
        "pwd should fall back to default_cwd"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// per-call `cwd` 优先于 `default_cwd`（017-b）。
#[tokio::test]
async fn test_bash_per_call_cwd_overrides_default() {
    let default_dir = temp_dir_unique();
    let call_dir = temp_dir_unique();
    std::fs::create_dir_all(&default_dir).expect("create default dir");
    std::fs::create_dir_all(&call_dir).expect("create call dir");
    let canonical = std::fs::canonicalize(&call_dir).expect("canonicalize dir");

    let tool = BashTool::new(Some(default_dir.clone()));
    let result = tool
        .execute(
            "c1",
            serde_json::json!({ "command": "pwd", "cwd": call_dir.to_string_lossy() }),
            CancellationToken::new(),
            None,
        )
        .await
        .expect("bash should succeed");
    assert!(!result.is_error);
    assert_eq!(
        text_of(&result).trim(),
        canonical.to_string_lossy(),
        "per-call cwd should override default_cwd"
    );

    let _ = std::fs::remove_dir_all(&default_dir);
    let _ = std::fs::remove_dir_all(&call_dir);
}
