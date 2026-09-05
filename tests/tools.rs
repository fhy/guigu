//! Task 005 集成测试：read/write/edit 文件工具。
//!
//! 以 `Arc<dyn Tool>` 走完整 trait 契约（name/description/parameters/resource_scope/
//! execute）+ 真实文件 IO。临时文件用 `std::env::temp_dir()` + 唯一后缀（进程 id +
//! 计数器），不硬编码路径，测试结束清理。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use guigu::core::message::ToolResultContent;
use guigu::core::tool::{ResourceScope, Tool, ToolError, ToolResult};
use guigu::tools::{EditTool, FileMutationQueue, ReadTool, WriteTool};
use tokio_util::sync::CancellationToken;

/// 生成唯一临时目录（进程 id + 计数器），保证测试间隔离。
fn temp_dir_unique() -> std::path::PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!("guigu-test-{}-{}", std::process::id(), n))
}

fn read_tool() -> Arc<dyn Tool> {
    Arc::new(ReadTool::new(None))
}

fn write_tool() -> Arc<dyn Tool> {
    Arc::new(WriteTool::new(Arc::new(FileMutationQueue::new()), None))
}

fn edit_tool() -> Arc<dyn Tool> {
    Arc::new(EditTool::new(Arc::new(FileMutationQueue::new()), None))
}

/// 用未取消的 signal 执行工具（测试统一入口）。
async fn run(tool: &Arc<dyn Tool>, args: serde_json::Value) -> Result<ToolResult, ToolError> {
    tool.execute("call1", args, CancellationToken::new(), None)
        .await
}

/// 取结果中唯一的 Text 内容。
fn text_of(result: &ToolResult) -> String {
    match &result.content[0] {
        ToolResultContent::Text { text } => text.clone(),
        other => panic!("expected Text content, got {other:?}"),
    }
}

// ---------- 契约测试（Arc<dyn Tool> 完整契约） ----------

/// read：name/description/parameters/resource_scope 契约。
#[test]
fn test_read_contract() {
    let tool = read_tool();
    assert_eq!(tool.name(), "read");
    assert!(!tool.description().is_empty());
    assert!(tool.parameters().is_some());
    assert_eq!(tool.resource_scope(), ResourceScope::ReadOnly);
}

/// write：name/description/parameters/resource_scope 契约。
#[test]
fn test_write_contract() {
    let tool = write_tool();
    assert_eq!(tool.name(), "write");
    assert!(!tool.description().is_empty());
    assert!(tool.parameters().is_some());
    assert_eq!(tool.resource_scope(), ResourceScope::FileWriter);
}

/// edit：name/description/parameters/resource_scope 契约。
#[test]
fn test_edit_contract() {
    let tool = edit_tool();
    assert_eq!(tool.name(), "edit");
    assert!(!tool.description().is_empty());
    assert!(tool.parameters().is_some());
    assert_eq!(tool.resource_scope(), ResourceScope::FileWriter);
}

// ---------- ReadTool IO 测试 ----------

/// read：读存在文件返回内容，details 含 path/bytes。
#[tokio::test]
async fn test_read_existing_file() {
    let dir = temp_dir_unique();
    let path = dir.join("read.txt");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    std::fs::write(&path, "hello world").expect("write temp file");

    let result = run(
        &read_tool(),
        serde_json::json!({ "path": path.to_string_lossy() }),
    )
    .await
    .expect("read should succeed");

    assert!(!result.is_error);
    assert_eq!(text_of(&result), "hello world");
    let details = result.details.as_ref().expect("details should be present");
    assert_eq!(details["bytes"], 11);
    assert_eq!(
        details["path"].as_str(),
        Some(path.to_string_lossy().as_ref())
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// read：不存在文件 → IO 错误（非取消错误）。
#[tokio::test]
async fn test_read_nonexistent_file() {
    let dir = temp_dir_unique();
    let path = dir.join("missing.txt");
    // 不创建 dir，path 必然不存在。

    let result = run(
        &read_tool(),
        serde_json::json!({ "path": path.to_string_lossy() }),
    )
    .await;
    match result {
        Err(e) => {
            assert!(
                !e.message.contains("cancelled"),
                "should be an IO error, got: {}",
                e.message
            )
        }
        Ok(_) => panic!("should fail for non-existent file"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// read：offset/limit 字节切片生效。
#[tokio::test]
async fn test_read_offset_limit() {
    let dir = temp_dir_unique();
    let path = dir.join("slice.txt");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    std::fs::write(&path, "hello world").expect("write temp file");
    let tool = read_tool();

    // offset=6, limit=5 → "world"
    let result = run(
        &tool,
        serde_json::json!({ "path": path.to_string_lossy(), "offset": 6, "limit": 5 }),
    )
    .await
    .expect("read should succeed");
    assert_eq!(text_of(&result), "world");
    let details = result.details.as_ref().expect("details should be present");
    assert_eq!(details["offset"], 6);
    assert_eq!(details["limit"], 5);

    // offset=6, 无 limit → "world"
    let result = run(
        &tool,
        serde_json::json!({ "path": path.to_string_lossy(), "offset": 6 }),
    )
    .await
    .expect("read should succeed");
    assert_eq!(text_of(&result), "world");

    // offset 超出文件长度 → 空内容（不 panic）
    let result = run(
        &tool,
        serde_json::json!({ "path": path.to_string_lossy(), "offset": 100 }),
    )
    .await
    .expect("read should succeed");
    assert_eq!(text_of(&result), "");

    let _ = std::fs::remove_dir_all(&dir);
}

/// read：非法 UTF-8 → IO 错误。
#[tokio::test]
async fn test_read_invalid_utf8() {
    let dir = temp_dir_unique();
    let path = dir.join("binary.bin");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    std::fs::write(&path, [0xffu8, 0xfe, 0xfd]).expect("write binary file");

    let result = run(
        &read_tool(),
        serde_json::json!({ "path": path.to_string_lossy() }),
    )
    .await;
    match result {
        Err(e) => {
            assert!(
                e.message.contains("UTF-8"),
                "should be a UTF-8 error, got: {}",
                e.message
            )
        }
        Ok(_) => panic!("should fail for invalid UTF-8"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// read：path 类型不符（数字）→ invalid_arguments。
#[tokio::test]
async fn test_read_type_mismatch() {
    let result = run(&read_tool(), serde_json::json!({ "path": 123 })).await;
    match result {
        Err(e) => {
            assert!(
                !e.message.contains("cancelled"),
                "should be invalid_arguments, got: {}",
                e.message
            )
        }
        Ok(_) => panic!("should fail for type mismatch"),
    }
}

// ---------- WriteTool IO 测试 ----------

/// write：新建文件成功，details 含 path/bytes。
#[tokio::test]
async fn test_write_new_file() {
    let dir = temp_dir_unique();
    let path = dir.join("new.txt");

    let result = run(
        &write_tool(),
        serde_json::json!({ "path": path.to_string_lossy(), "content": "hello" }),
    )
    .await
    .expect("write should succeed");

    assert!(!result.is_error);
    let on_disk = std::fs::read_to_string(&path).expect("file should exist");
    assert_eq!(on_disk, "hello");
    let details = result.details.as_ref().expect("details should be present");
    assert_eq!(details["bytes"], 5);

    let _ = std::fs::remove_dir_all(&dir);
}

/// write：覆盖写已有文件成功。
#[tokio::test]
async fn test_write_overwrite() {
    let dir = temp_dir_unique();
    let path = dir.join("overwrite.txt");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    std::fs::write(&path, "old content").expect("write initial file");

    let result = run(
        &write_tool(),
        serde_json::json!({ "path": path.to_string_lossy(), "content": "new" }),
    )
    .await
    .expect("write should succeed");

    assert!(!result.is_error);
    let on_disk = std::fs::read_to_string(&path).expect("file should exist");
    assert_eq!(on_disk, "new");

    let _ = std::fs::remove_dir_all(&dir);
}

/// write：父目录不存在时自动创建。
#[tokio::test]
async fn test_write_creates_parent_dirs() {
    let dir = temp_dir_unique();
    let path = dir.join("sub").join("deep").join("file.txt");

    let result = run(
        &write_tool(),
        serde_json::json!({ "path": path.to_string_lossy(), "content": "nested" }),
    )
    .await
    .expect("write should succeed");

    assert!(!result.is_error);
    let on_disk = std::fs::read_to_string(&path).expect("nested file should exist");
    assert_eq!(on_disk, "nested");

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------- EditTool IO 测试 ----------

/// edit：唯一匹配替换成功，details 含 replaced=1。
#[tokio::test]
async fn test_edit_unique_match() {
    let dir = temp_dir_unique();
    let path = dir.join("edit.txt");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    std::fs::write(&path, "foo bar foo2 baz").expect("write temp file");

    let result = run(
        &edit_tool(),
        serde_json::json!({ "path": path.to_string_lossy(), "old_string": "bar", "new_string": "BAZ" }),
    )
    .await
    .expect("edit should succeed");

    assert!(!result.is_error);
    let on_disk = std::fs::read_to_string(&path).expect("file should exist");
    assert_eq!(on_disk, "foo BAZ foo2 baz");
    let details = result.details.as_ref().expect("details should be present");
    assert_eq!(details["replaced"], 1);

    let _ = std::fs::remove_dir_all(&dir);
}

/// edit：old_string 不存在 → 错误。
#[tokio::test]
async fn test_edit_not_found() {
    let dir = temp_dir_unique();
    let path = dir.join("edit2.txt");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    std::fs::write(&path, "hello world").expect("write temp file");

    let result = run(
        &edit_tool(),
        serde_json::json!({ "path": path.to_string_lossy(), "old_string": "not-there", "new_string": "x" }),
    )
    .await;
    match result {
        Err(e) => {
            assert!(
                e.message.contains("not found"),
                "should be a not-found error, got: {}",
                e.message
            )
        }
        Ok(_) => panic!("should fail when old_string not found"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// edit：多处匹配 → 错误。
#[tokio::test]
async fn test_edit_multiple_matches() {
    let dir = temp_dir_unique();
    let path = dir.join("edit3.txt");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    std::fs::write(&path, "a b a c a").expect("write temp file");

    let result = run(
        &edit_tool(),
        serde_json::json!({ "path": path.to_string_lossy(), "old_string": "a", "new_string": "z" }),
    )
    .await;
    match result {
        Err(e) => {
            assert!(
                e.message.contains("not unique"),
                "should be a not-unique error, got: {}",
                e.message
            )
        }
        Ok(_) => panic!("should fail when old_string not unique"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------- work_dir 工作目录隔离测试（017-b） ----------

/// read + work_dir：相对路径 join work_dir 后读取（经 `Arc<dyn Tool>` 契约）。
#[tokio::test]
async fn test_read_work_dir_relative() {
    let dir = temp_dir_unique();
    std::fs::create_dir_all(&dir).expect("create temp dir");
    std::fs::write(dir.join("wd.txt"), "work dir content").expect("write temp file");

    let tool: Arc<dyn Tool> = Arc::new(ReadTool::new(Some(dir.clone())));
    let result = run(&tool, serde_json::json!({ "path": "wd.txt" }))
        .await
        .expect("read should succeed");
    assert_eq!(text_of(&result), "work dir content");

    let _ = std::fs::remove_dir_all(&dir);
}

/// write + work_dir：相对路径 join work_dir 后写入（经 `Arc<dyn Tool>` 契约）。
#[tokio::test]
async fn test_write_work_dir_relative() {
    let dir = temp_dir_unique();
    std::fs::create_dir_all(&dir).expect("create temp dir");

    let tool: Arc<dyn Tool> = Arc::new(WriteTool::new(
        Arc::new(FileMutationQueue::new()),
        Some(dir.clone()),
    ));
    let result = run(
        &tool,
        serde_json::json!({ "path": "nested/wd.txt", "content": "hello" }),
    )
    .await
    .expect("write should succeed");
    assert!(!result.is_error);
    let on_disk = std::fs::read_to_string(dir.join("nested/wd.txt"))
        .expect("file should exist under work_dir");
    assert_eq!(on_disk, "hello");

    let _ = std::fs::remove_dir_all(&dir);
}

/// edit + work_dir：相对路径 join work_dir 后编辑（经 `Arc<dyn Tool>` 契约）。
#[tokio::test]
async fn test_edit_work_dir_relative() {
    let dir = temp_dir_unique();
    std::fs::create_dir_all(&dir).expect("create temp dir");
    std::fs::write(dir.join("wd.txt"), "foo bar").expect("write temp file");

    let tool: Arc<dyn Tool> = Arc::new(EditTool::new(
        Arc::new(FileMutationQueue::new()),
        Some(dir.clone()),
    ));
    let result = run(
        &tool,
        serde_json::json!({ "path": "wd.txt", "old_string": "bar", "new_string": "BAZ" }),
    )
    .await
    .expect("edit should succeed");
    assert!(!result.is_error);
    let on_disk =
        std::fs::read_to_string(dir.join("wd.txt")).expect("file should exist under work_dir");
    assert_eq!(on_disk, "foo BAZ");

    let _ = std::fs::remove_dir_all(&dir);
}

/// work_dir 隔离：两个工具不同 work_dir，同名相对路径落到不同文件（session 间隔离）。
#[tokio::test]
async fn test_work_dir_isolation() {
    let dir_a = temp_dir_unique();
    let dir_b = temp_dir_unique();
    std::fs::create_dir_all(&dir_a).expect("create dir a");
    std::fs::create_dir_all(&dir_b).expect("create dir b");

    let tool_a: Arc<dyn Tool> = Arc::new(WriteTool::new(
        Arc::new(FileMutationQueue::new()),
        Some(dir_a.clone()),
    ));
    let tool_b: Arc<dyn Tool> = Arc::new(WriteTool::new(
        Arc::new(FileMutationQueue::new()),
        Some(dir_b.clone()),
    ));
    run(
        &tool_a,
        serde_json::json!({ "path": "same.txt", "content": "A" }),
    )
    .await
    .expect("write a");
    run(
        &tool_b,
        serde_json::json!({ "path": "same.txt", "content": "B" }),
    )
    .await
    .expect("write b");

    assert_eq!(
        std::fs::read_to_string(dir_a.join("same.txt")).expect("a exists"),
        "A"
    );
    assert_eq!(
        std::fs::read_to_string(dir_b.join("same.txt")).expect("b exists"),
        "B"
    );

    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}
