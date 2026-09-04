//! CLI 冒烟测试（Task 015）。
//!
//! 用 `std::process::Command` / `tokio::process::Command` 跑 `guigu` 二进制：
//! - `--help` 正常打印且退出码 0；
//! - `run` 缺 API key → 非零退出 + stderr 提示（不依赖外网）；
//! - `run`（fake provider）：prompt → 事件流（text）打印 → `/quit` 退出；
//! - `run`（fake provider）：`--session` 续聊（load_session）与新建路径都可跑；
//! - `acp` stdio loopback：initialize → session/new → session/prompt 往返
//!   （复用 014 wire 约定，不依赖编辑器）。
//!
//! 真 adapter（OpenAI/Anthropic）需网络，不在本测试覆盖（避免依赖外网）；
//! 离线冒烟用隐藏的 `--provider fake`（规格验收「测试用 fake/offline provider
//! 冒烟，避免依赖外网」）。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Output, Stdio};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;

/// `guigu --help` 正常打印且退出码 0。
#[test]
fn test_help_exits_zero() {
    let output = StdCommand::new(env!("CARGO_BIN_EXE_guigu"))
        .arg("--help")
        .output()
        .expect("run guigu --help");
    assert!(output.status.success(), "guigu --help should exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("guigu"), "help should mention guigu");
    assert!(stdout.contains("run"), "help should mention run command");
    assert!(stdout.contains("acp"), "help should mention acp command");
    assert!(stdout.contains("--model"), "help should mention --model");
    assert!(
        stdout.contains("--provider"),
        "help should mention --provider"
    );
    assert!(
        stdout.contains("--session"),
        "help should mention --session"
    );
}

/// `guigu run` 缺 API key → 非零退出 + stderr 提示（不依赖外网）。
#[test]
fn test_run_missing_api_key() {
    let output = StdCommand::new(env!("CARGO_BIN_EXE_guigu"))
        .arg("run")
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .expect("run guigu run");
    assert!(
        !output.status.success(),
        "guigu run without API key should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("missing API key"),
        "should mention missing API key, got: {stderr}"
    );
}

/// REPL 冒烟（fake provider）：prompt → 事件流（text）打印 → `/quit` 退出。
///
/// 覆盖验收「输入 prompt → 事件流（text/tool）打印 → run 结束；`/quit` 正常退出」。
#[test]
fn test_repl_fake_provider_prompt_and_quit() {
    let log_dir = tempfile::tempdir().expect("tempdir");
    let output = run_repl_once(log_dir.path(), None, "hello");
    assert!(
        output.status.success(),
        "guigu run should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("ok"),
        "REPL should print fake provider response 'ok', got: {stdout}"
    );
}

/// `--session` 续聊（load_session）与新建路径都可跑（fake provider）。
///
/// A. 新建：无 `--session`（auto-id，`create_session`）→ JSONL 落盘。
/// B. 续聊：`--session <id>` 先建（文件不存在 → load 空树）再续（文件存在 →
///    load 既有树）→ 两次都退出 0，JSONL 逐次增长。
#[test]
fn test_repl_session_new_and_resume() {
    let log_dir = tempfile::tempdir().expect("tempdir");

    // A. 新建路径（无 --session，auto-id）。
    let out_new = run_repl_once(log_dir.path(), None, "hello");
    assert!(
        out_new.status.success(),
        "new session should exit 0, stderr: {}",
        String::from_utf8_lossy(&out_new.stderr)
    );
    let jsonls: Vec<PathBuf> = list_jsonl(log_dir.path());
    assert!(
        jsonls.len() == 1,
        "exactly one JSONL should be created, got: {jsonls:?}"
    );
    assert!(
        count_jsonl_lines(&jsonls[0]) > 0,
        "new session should persist entries"
    );

    // B. 续聊路径（--session <id>，load_session）。
    let session_id = "s-015-resume";
    let resume_jsonl = log_dir.path().join(format!("{session_id}.jsonl"));
    let out_b1 = run_repl_once(log_dir.path(), Some(session_id), "hi");
    assert!(
        out_b1.status.success(),
        "resume-create should exit 0, stderr: {}",
        String::from_utf8_lossy(&out_b1.stderr)
    );
    let entries_b1 = count_jsonl_lines(&resume_jsonl);
    assert!(entries_b1 > 0, "resume-create should persist entries");
    let out_b2 = run_repl_once(log_dir.path(), Some(session_id), "again");
    assert!(
        out_b2.status.success(),
        "resume should exit 0, stderr: {}",
        String::from_utf8_lossy(&out_b2.stderr)
    );
    let entries_b2 = count_jsonl_lines(&resume_jsonl);
    assert!(
        entries_b2 > entries_b1,
        "resume should append entries ({} -> {})",
        entries_b1,
        entries_b2
    );
}

/// `guigu acp` stdio loopback：initialize → session/new 往返（不依赖编辑器）。
///
/// 用 fake API key（provider 在 `session/prompt` 才调用，故 `initialize` /
/// `session/new` 不需网络）；`--log` 指向临时目录（避免污染真实 state）。
#[tokio::test]
async fn test_acp_loopback_initialize_and_new() {
    let log_dir = tempfile::tempdir().expect("tempdir");

    let mut child = TokioCommand::new(env!("CARGO_BIN_EXE_guigu"))
        .arg("acp")
        .arg("--api-key")
        .arg("fake-key")
        .arg("--log")
        .arg(log_dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn guigu acp");

    let mut stdin = child.stdin.take().expect("take stdin");
    let stdout = child.stdout.take().expect("take stdout");
    let mut reader = BufReader::new(stdout);

    // initialize。
    write_jsonrpc(
        &mut stdin,
        Some(1),
        "initialize",
        serde_json::json!({"protocolVersion": 1}),
    )
    .await;
    let resp = read_jsonrpc(&mut reader).await;
    assert_eq!(resp["id"], 1, "initialize response id");
    assert_eq!(resp["result"]["protocolVersion"], 1, "protocolVersion");
    assert_eq!(
        resp["result"]["agentInfo"]["name"], "guigu",
        "agentInfo.name"
    );

    // session/new。
    write_jsonrpc(&mut stdin, Some(2), "session/new", serde_json::json!({})).await;
    let resp = read_jsonrpc(&mut reader).await;
    assert_eq!(resp["id"], 2, "session/new response id");
    assert!(
        resp["result"]["sessionId"].is_string(),
        "session/new should return sessionId"
    );

    // 清理：关闭 stdin（EOF）→ 进程退出。
    drop(stdin);
    let _ = child.wait().await;
}

/// `guigu acp` stdio loopback：initialize → session/new → session/prompt 完整往返。
///
/// 用隐藏的 `--provider fake`（离线，无网络）：prompt 返回 `stopReason: end_turn`
/// （`Completed` → `end_turn`），沿途收 `session/update` notification。复用 014
/// wire 约定，不依赖编辑器。
#[tokio::test]
async fn test_acp_loopback_prompt_roundtrip() {
    let log_dir = tempfile::tempdir().expect("tempdir");

    let mut child = TokioCommand::new(env!("CARGO_BIN_EXE_guigu"))
        .arg("acp")
        .arg("--provider")
        .arg("fake")
        .arg("--log")
        .arg(log_dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn guigu acp");

    let mut stdin = child.stdin.take().expect("take stdin");
    let stdout = child.stdout.take().expect("take stdout");
    let mut reader = BufReader::new(stdout);

    // initialize。
    write_jsonrpc(
        &mut stdin,
        Some(1),
        "initialize",
        serde_json::json!({"protocolVersion": 1}),
    )
    .await;
    let resp = read_jsonrpc(&mut reader).await;
    assert_eq!(resp["id"], 1, "initialize response id");

    // session/new。
    write_jsonrpc(&mut stdin, Some(2), "session/new", serde_json::json!({})).await;
    let resp = read_jsonrpc(&mut reader).await;
    let session_id = resp["result"]["sessionId"]
        .as_str()
        .expect("sessionId")
        .to_string();

    // session/prompt（双工：run 进行中逐条推 session/update notification）。
    write_jsonrpc(
        &mut stdin,
        Some(3),
        "session/prompt",
        serde_json::json!({
            "sessionId": session_id,
            "prompt": [{ "type": "text", "text": "hi" }]
        }),
    )
    .await;

    // 读消息直到 prompt 应答（id=3）到达；沿途收集 session/update。
    let mut updates = 0usize;
    let mut prompt_resp = None;
    while prompt_resp.is_none() {
        let msg = read_jsonrpc(&mut reader).await;
        if msg.get("method").and_then(serde_json::Value::as_str) == Some("session/update") {
            updates += 1;
        } else if msg.get("id").and_then(serde_json::Value::as_u64) == Some(3) {
            prompt_resp = Some(msg);
        }
    }

    // 至少一条 session/update（FakeProvider 发一个 TextDelta）。
    assert!(updates > 0, "should receive session/update notifications");
    let resp = prompt_resp.expect("prompt response");
    assert_eq!(
        resp["result"]["stopReason"], "end_turn",
        "stopReason should be end_turn"
    );

    // 清理：关闭 stdin（EOF）→ 进程退出。
    drop(stdin);
    let _ = child.wait().await;
}

/// 跑一次 `guigu run --provider fake [--session <id>]`：pipe 单条 prompt + `/quit`。
fn run_repl_once(log_dir: &Path, session_id: Option<&str>, prompt: &str) -> Output {
    let mut cmd = StdCommand::new(env!("CARGO_BIN_EXE_guigu"));
    cmd.args(["run", "--provider", "fake", "--log"])
        .arg(log_dir);
    if let Some(id) = session_id {
        cmd.args(["--session", id]);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn guigu run");
    {
        let mut stdin = child.stdin.take().expect("take stdin");
        let mut input = prompt.as_bytes().to_vec();
        input.push(b'\n');
        input.extend_from_slice(b"/quit\n");
        stdin.write_all(&input).expect("write stdin");
    }
    child.wait_with_output().expect("wait")
}

/// 列出目录下全部 `.jsonl` 文件。
fn list_jsonl(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .expect("read dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("jsonl"))
        .collect()
}

/// 统计 JSONL 文件行数。
fn count_jsonl_lines(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .expect("read jsonl")
        .lines()
        .count()
}

/// 写一条 JSON-RPC 请求（newline-delimited）。
async fn write_jsonrpc(
    stdin: &mut tokio::process::ChildStdin,
    id: Option<u64>,
    method: &str,
    params: serde_json::Value,
) {
    let mut msg = serde_json::json!({ "jsonrpc": "2.0", "method": method, "params": params });
    if let Some(id) = id {
        msg["id"] = serde_json::json!(id);
    }
    let mut line = serde_json::to_string(&msg).expect("serialize");
    line.push('\n');
    stdin.write_all(line.as_bytes()).await.expect("write");
    stdin.flush().await.expect("flush");
}

/// 读一条 JSON-RPC 应答。
async fn read_jsonrpc(reader: &mut BufReader<tokio::process::ChildStdout>) -> serde_json::Value {
    let mut line = String::new();
    let n = reader.read_line(&mut line).await.expect("read line");
    assert!(n > 0, "read 0 bytes (EOF)");
    serde_json::from_str(&line).expect("parse jsonrpc")
}
