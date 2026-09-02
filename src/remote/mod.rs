//! 远程协议（Task 010）：把 `AgentHandle` 的命令面 + 事件流序列化到字节流。
//!
//! - `protocol`：wire 协议类型（`RemoteRequest` / `ServerMessage` / `RemoteError`）
//! - `codec`：newline-delimited JSON 帧编解码
//! - `server`：`RemoteServer`（把 `AgentHandle` 暴露到字节流）
//! - `client`：`RemoteClient`（跨进程的 handle 等价物）
//!
//! 边界声明：单 agent / 单 server；多 client 并发、多 lane 并发写 session
//! 不在本任务（属后续，009 已声明）。

mod client;
pub mod codec;
mod protocol;
mod server;

pub use client::RemoteClient;
pub use protocol::{RemoteError, RemoteRequest, ServerMessage};
pub use server::RemoteServer;

// ---------- Connector ----------
//
// 只负责「产出满足 codec 泛型约束（AsyncRead + AsyncWrite + Send + Unpin + 'static）
// 的字节流」，协议 / codec 层不感知具体 transport。

use std::io;
use std::pin::Pin;
use std::process::Stdio;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream, ToSocketAddrs};
use tokio::process::{ChildStdin, ChildStdout};
use tokio_util::sync::CancellationToken;

/// 把一个读半与一个写半组合为单一双工流（满足 codec 的 `AsyncRead + AsyncWrite`
/// 泛型约束）。`tokio::io::split` 的逆操作。
pub struct SplitDuplex<R, W> {
    read: R,
    write: W,
}

impl<R, W> SplitDuplex<R, W> {
    /// 由读半 + 写半构造。
    pub fn new(read: R, write: W) -> Self {
        Self { read, write }
    }
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> AsyncRead for SplitDuplex<R, W> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        AsyncRead::poll_read(Pin::new(&mut self.read), cx, buf)
    }
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> AsyncWrite for SplitDuplex<R, W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        AsyncWrite::poll_write(Pin::new(&mut self.write), cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        AsyncWrite::poll_flush(Pin::new(&mut self.write), cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        AsyncWrite::poll_shutdown(Pin::new(&mut self.write), cx)
    }
}

/// 当前进程 stdin/stdout 的双工流（服务端侧：直接对 `tokio::io::stdin()/stdout()`
/// 跑 `serve`）。
pub fn process_stdio() -> SplitDuplex<tokio::io::Stdin, tokio::io::Stdout> {
    SplitDuplex::new(tokio::io::stdin(), tokio::io::stdout())
}

/// stdio connector：启动 agent 子进程，返回双工流。
///
/// 客户端侧以 `tokio::process::Command` 启动 agent 子进程，取 stdin/stdout
/// 组合为双工流。drop 时 kill 子进程并异步回收（`kill().await` 后
/// `wait().await`），避免 zombie。
pub async fn spawn_stdio(cmd: &mut tokio::process::Command) -> Result<StdioStream, RemoteError> {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| RemoteError::Protocol("stdin not piped".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| RemoteError::Protocol("stdout not piped".into()))?;
    let pid = child.id();

    // 子进程回收 task：等待子进程自然退出，或 drop 触发 kill + wait（回收）。
    let token = CancellationToken::new();
    let token_clone = token.clone();
    tokio::spawn(async move {
        tokio::select! {
            result = child.wait() => {
                // 子进程自然退出（已回收）。
                let _ = result;
            }
            _ = token_clone.cancelled() => {
                // drop 触发：kill 子进程并等待其退出（回收，避免 zombie）。
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
        }
    });

    Ok(StdioStream {
        stdin,
        stdout,
        pid,
        token,
    })
}

/// tcp connector：连接 TCP 服务器，返回双工流（`TcpStream` 天然实现
/// `AsyncRead + AsyncWrite`）。
pub async fn connect_tcp(addr: impl ToSocketAddrs) -> Result<TcpStream, RemoteError> {
    TcpStream::connect(addr).await.map_err(RemoteError::Io)
}

/// tcp connector：在 TCP 地址监听并接受一条连接，返回双工流。
pub async fn listen_tcp(addr: impl ToSocketAddrs) -> Result<TcpStream, RemoteError> {
    let listener = TcpListener::bind(addr).await?;
    let (stream, _) = listener.accept().await?;
    Ok(stream)
}

/// stdio 双工流：组合子进程的 stdin/stdout。
///
/// 读侧委托 `stdout`，写侧委托 `stdin`。drop 时 kill 子进程并异步回收
/// （`kill().await` 后 `wait().await`），避免 zombie。
pub struct StdioStream {
    stdin: ChildStdin,
    stdout: ChildStdout,
    /// 子进程 PID（用于测试验证回收）。
    pid: Option<u32>,
    /// 子进程回收 task 的取消信号（drop 时触发 kill + wait）。
    token: CancellationToken,
}

impl StdioStream {
    /// 获取子进程 PID。
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }
}

impl AsyncRead for StdioStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        AsyncRead::poll_read(Pin::new(&mut self.stdout), cx, buf)
    }
}

impl AsyncWrite for StdioStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        AsyncWrite::poll_write(Pin::new(&mut self.stdin), cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        AsyncWrite::poll_flush(Pin::new(&mut self.stdin), cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        AsyncWrite::poll_shutdown(Pin::new(&mut self.stdin), cx)
    }
}

impl Drop for StdioStream {
    fn drop(&mut self) {
        // 触发 kill 信号（回收 task 会 kill + wait，避免 zombie）。
        self.token.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// 回归（问题4）：drop StdioStream 后，子进程被 kill 并回收（无 zombie）。
    #[tokio::test]
    async fn test_stdio_stream_reaps_child() {
        // Spawn a child process that sleeps.
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg("10");
        let stream = spawn_stdio(&mut cmd).await.expect("spawn");
        let pid = stream.pid().expect("pid available after spawn");

        // Drop the stream (triggers kill + reap).
        drop(stream);

        // Wait for the reaping task to complete.
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Verify the child process is gone (no zombie).
        // On Linux, a reaped process has no /proc/<pid>/ entry.
        let proc_path = format!("/proc/{pid}");
        assert!(
            !std::path::Path::new(&proc_path).exists(),
            "child process should be reaped (no /proc/{pid} entry)"
        );
    }
}
