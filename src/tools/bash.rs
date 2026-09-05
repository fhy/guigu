//! BashTool：以 `sh -c` 执行命令（`Exclusive` 范围）。
//!
//! 真实子进程执行 + 取消 kill + 超时。非零退出码不 throw（Pi 哲学"错误不
//! throw"）：返回 `ToolResult { is_error: true }`，退出码/stdout/stderr 进
//! details。取消/超时显式 `kill().await` 后 `wait().await` 严格 reap，配合
//! `kill_on_drop(true)` 兜底，不泄漏僵尸进程。
//!
//! 禁用 `wait_with_output`：它按值消费 `Child`（`mut self`），`select!` 急切建
//! future 后另两分支 `child.kill()` 即 use-after-move；改用 `child.wait()`
//! （`&mut self`）+ 提前 take 管道 + spawn 排空。
//!
//! 017-b：构造注入 `default_cwd`，`BashArgs.cwd`（per-call 参数）为空时回退
//! `default_cwd`（装配层填充，session 间隔离）；per-call `cwd` 优先。
//!
//! 单元测试见 `tests/bash.rs`（走完整 `Tool` trait 契约）。

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

use crate::core::message::ToolResultContent;
use crate::core::tool::{ResourceScope, Tool, ToolError, ToolResult};

/// BashTool 参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BashArgs {
    /// 要执行的命令（以 `sh -c` 解释，支持管道/重定向）。
    pub command: String,
    /// 工作目录（可选）。
    pub cwd: Option<String>,
    /// 超时毫秒数（可选，最小 1）。
    pub timeout_ms: Option<u64>,
}

/// 命令执行工具：以 `sh -c <command>` 启动子进程，支持取消与超时。
///
/// 017-b：构造注入 `default_cwd`，`BashArgs.cwd`（per-call 参数）为空时回退
/// `default_cwd`（装配层填充，session 间隔离）；per-call `cwd` 优先。
#[derive(Debug, Clone)]
pub struct BashTool {
    default_cwd: Option<PathBuf>,
}

impl BashTool {
    /// 注入默认工作目录（`BashArgs.cwd` 为空时的回退；`None` = 进程 cwd）。
    pub fn new(default_cwd: Option<PathBuf>) -> Self {
        BashTool { default_cwd }
    }

    /// 组装 `sh -c` 子进程命令：管道化 stdout/stderr、`kill_on_drop` 防泄漏。
    ///
    /// cwd 取 per-call `args.cwd` 优先，为空（`None` / 空串）时回退
    /// `default_cwd`（017-b）。
    fn build_command(&self, args: &BashArgs) -> Command {
        let mut cmd = Command::new("sh");
        cmd.arg("-c");
        cmd.arg(&args.command);
        cmd.kill_on_drop(true);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        // per-call cwd 优先；为空时回退 default_cwd（017-b）。
        if let Some(cwd) = args.cwd.as_deref().filter(|c| !c.is_empty()) {
            cmd.current_dir(cwd);
        } else if let Some(dir) = &self.default_cwd {
            cmd.current_dir(dir);
        }
        cmd
    }

    /// 取消/超时路径：显式 kill 后 wait 严格 reap（不依赖 best-effort reaper）。
    async fn kill_and_reap(child: &mut Child) {
        if let Err(e) = child.kill().await {
            tracing::warn!("bash: kill failed (process may have exited): {e}");
        }
        if let Err(e) = child.wait().await {
            tracing::warn!("bash: reap after kill failed: {e}");
        }
    }

    /// join 排空任务：JoinError 与 read 错误均走 `ToolError::new`。
    async fn join_drain(
        task: Option<tokio::task::JoinHandle<std::io::Result<Vec<u8>>>>,
        label: &str,
    ) -> Result<String, ToolError> {
        let bytes = match task {
            Some(t) => t
                .await
                .map_err(|e| ToolError::new(format!("bash: join {label} drain task: {e}")))?
                .map_err(|e| ToolError::new(format!("bash: read {label}: {e}")))?,
            None => Vec::new(),
        };
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// 按退出码组装结果：exit 0 → 文本结果；非零 → `is_error` + details。
    fn assemble_result(
        status: &std::process::ExitStatus,
        stdout: String,
        stderr: String,
    ) -> ToolResult {
        let code = status.code().map(|c| c as i64).unwrap_or(-1);
        if code == 0 {
            let mut result = ToolResult::text(stdout);
            result.details = Some(serde_json::json!({ "exit_code": 0 }));
            return result;
        }
        let content_text = if stdout.is_empty() && stderr.is_empty() {
            String::new()
        } else if stderr.is_empty() {
            stdout.clone()
        } else if stdout.is_empty() {
            stderr.clone()
        } else {
            format!("{stdout}\n{stderr}")
        };
        ToolResult {
            content: vec![ToolResultContent::Text { text: content_text }],
            is_error: true,
            details: Some(serde_json::json!({
                "exit_code": code,
                "stdout": stdout,
                "stderr": stderr,
            })),
        }
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Run a shell command via `sh -c`. Supports cwd and timeout_ms. Non-zero exit returns an error result (not a throw)."
    }

    fn parameters(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "command":    { "type": "string" },
                "cwd":        { "type": "string" },
                "timeout_ms": { "type": "integer", "minimum": 1 }
            },
            "required": ["command"]
        }))
    }

    fn resource_scope(&self) -> ResourceScope {
        ResourceScope::Exclusive
    }

    async fn execute(
        &self,
        _tool_call_id: &str,
        args: serde_json::Value,
        signal: CancellationToken,
        _on_update: Option<&(dyn Fn(ToolResult) + Send + Sync)>,
    ) -> Result<ToolResult, ToolError> {
        if signal.is_cancelled() {
            return Err(ToolError::new(
                "cancelled: bash aborted before spawn".to_string(),
            ));
        }

        let bash_args: BashArgs = serde_json::from_value(args)
            .map_err(|e| ToolError::invalid_arguments(e.to_string()))?;

        let mut child = self
            .build_command(&bash_args)
            .spawn()
            .map_err(|e| ToolError::new(format!("bash spawn: {e}")))?;

        // 提前 take 管道并 spawn 排空，避免管道缓冲写满导致子进程死锁。
        let stdout_task = child.stdout.take().map(|mut p| {
            tokio::spawn(async move {
                let mut buf = Vec::new();
                p.read_to_end(&mut buf).await.map(|_| buf)
            })
        });
        let stderr_task = child.stderr.take().map(|mut p| {
            tokio::spawn(async move {
                let mut buf = Vec::new();
                p.read_to_end(&mut buf).await.map(|_| buf)
            })
        });

        // 三路 select：wait 完成 / 取消 / 超时。仅 wait 分支 future 持有 &mut child，
        // 取消/超时分支胜出时该 future 已 drop，故可在分支内对 child kill+wait。
        let wait_result = match bash_args.timeout_ms {
            Some(ms) => {
                let sleep = tokio::time::sleep(Duration::from_millis(ms));
                tokio::select! {
                    r = child.wait() => r,
                    _ = signal.cancelled() => {
                        Self::kill_and_reap(&mut child).await;
                        return Err(ToolError::new(
                            "cancelled: bash killed".to_string(),
                        ));
                    }
                    _ = sleep => {
                        Self::kill_and_reap(&mut child).await;
                        return Err(ToolError::new(format!(
                            "timeout: bash killed after {ms}ms"
                        )));
                    }
                }
            }
            None => {
                tokio::select! {
                    r = child.wait() => r,
                    _ = signal.cancelled() => {
                        Self::kill_and_reap(&mut child).await;
                        return Err(ToolError::new(
                            "cancelled: bash killed".to_string(),
                        ));
                    }
                }
            }
        };
        let status = wait_result.map_err(|e| ToolError::new(format!("bash wait: {e}")))?;

        let stdout = Self::join_drain(stdout_task, "stdout").await?;
        let stderr = Self::join_drain(stderr_task, "stderr").await?;

        Ok(Self::assemble_result(&status, stdout, stderr))
    }
}
