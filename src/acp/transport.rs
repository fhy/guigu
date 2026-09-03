//! ACP stdio 传输（Task 014）：JSON-RPC 2.0 over stdio（1 进程 = 1 client）。
//!
//! 分帧：newline-delimited JSON（ACP 官方 stdio 约定，UTF-8、无内嵌换行）。复用
//! 010 `remote::codec`（`LineReader` / `write_line`）。
//!
//! 双工：读循环对每条入站消息——
//! - **请求 / notification**（有 `method`）：spawn 独立 task 调 `AcpAgent::handle`，
//!   请求（有 `id`）回 JSON-RPC 应答，notification（无 `id`）不应答。spawn 独立
//!   task 使 `session/prompt`（阻塞至 run 结束）与 `session/cancel`（中止 lane）可并发。
//! - **应答**（无 `method`、有 `id`）：路由到 `StdioClient` 的 pending 请求。
//!
//! 写半归单一 writer task（`mpsc` 汇入 → 写循环），避免多任务持锁跨 await。
//! **writer 写失败**经 oneshot 通知读循环（Issue 1）：读循环 `select` 该错误与
//! 输入，统一执行 `shutdown.cancel()` + `client.cancel_all()` + handler 回收，
//! 返回明确 IO 错误，避免 `serve_connection` 永久阻塞、pending 请求泄漏。
//!
//! handler task 用 `JoinSet` 承载（Issue 2/4）：主循环持续 `try_join_next()` 回收
//! 已完成任务（避免长连接无界增长），并观察 `JoinError`（panic 不静默）。
//!
//! 模块拆分（单文件 ≤ 400 行约束）：JSON-RPC 类型 / 分类见 `jsonrpc`，
//! `StdioClient` / `StdioConnection` 见 `stdio_client`。
//!
//! SSE+HTTP 为可选加分项（`acp-sse` feature），本任务降级为后续（`serve_sse` 存根）。

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::acp::jsonrpc::{InboundKind, InboundMessage, OutboundMessage, classify_inbound};
use crate::acp::stdio_client::{StdioClient, StdioConnection};
use crate::acp::{AcpAgent, AcpError};
use crate::remote::RemoteError;
use crate::remote::codec::{LineReader, write_line};

impl AcpAgent {
    /// stdio：对 stdin/stdout 跑 JSON-RPC 2.0（1 进程 = 1 client）。
    ///
    /// 读循环 dispatch 入站消息；写半归单一 writer task。EOF（client 断开）时返回。
    pub async fn serve_stdio(self) -> Result<(), AcpError> {
        self.serve_connection(tokio::io::stdin(), tokio::io::stdout())
            .await
    }

    /// 对任意 `AsyncRead` / `AsyncWrite` 跑 JSON-RPC 2.0（stdio / 测试 duplex 通用）。
    ///
    /// 读循环 dispatch 入站消息；写半归单一 writer task。EOF（client 断开）时返回。
    pub async fn serve_connection<R, W>(self, reader: R, writer: W) -> Result<(), AcpError>
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
        W: tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        self.serve_connection_with(reader, writer, StdioConnection::new())
            .await
    }

    /// 用注入的连接资源跑 JSON-RPC 2.0（测试 / 嵌入方可注入 `StdioClient`）。
    ///
    /// 与 `serve_connection` 相同，但 `StdioClient` 由 `conn` 提供，调用方可经
    /// `conn.client()` 取得句柄驱动 agent→client 请求（如测试 writer 错误路径下
    /// 的 pending 清理）。
    pub async fn serve_connection_with<R, W>(
        self,
        reader: R,
        writer: W,
        conn: StdioConnection,
    ) -> Result<(), AcpError>
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
        W: tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (client, mut writer_rx) = conn.into_parts();
        let mut reader = LineReader::new(reader);
        let mut writer = writer;

        // 写半归单一 writer task：mpsc 汇入 → 写循环。首个写错误经 mpsc 通知读循环
        // （Issue 1）；EOF / 错误时由读循环 `shutdown.cancel()` 兜底退出。
        // 用 mpsc（容量 1）而非 oneshot：mpsc receiver 是 `Unpin`，可在 `select!`
        // 循环中复用；oneshot receiver 非 `Unpin`，循环内会被移动导致编译失败。
        let (writer_error_tx, mut writer_error_rx) = mpsc::channel::<AcpError>(1);
        let shutdown = CancellationToken::new();
        let writer_shutdown = shutdown.clone();
        let writer_task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    msg = writer_rx.recv() => match msg {
                        Some(msg) => {
                            if let Err(e) = write_line(&mut writer, &msg).await {
                                tracing::warn!("acp stdio: write failed: {e}");
                                // 首个写错误通知读循环：读循环据此统一清理并返回 IO 错误。
                                let acp_err = match e {
                                    RemoteError::Io(io_err) => AcpError::Io(io_err),
                                    other => AcpError::JsonRpc(other.to_string()),
                                };
                                let _ = writer_error_tx.send(acp_err).await;
                                break;
                            }
                        }
                        None => break,
                    },
                    _ = writer_shutdown.cancelled() => break,
                }
            }
        });

        let agent = Arc::new(self);
        // in-flight handler task（JoinSet 持续回收，避免无界增长 + 观察 panic，Issue 2/4）。
        let mut handlers = JoinSet::new();

        // 读循环：select 入站消息 与 writer 错误。返回 `Ok(())`（EOF）或 `Err`。
        let result: Result<(), AcpError> = loop {
            // 先回收已完成的 handler task（Issue 2/4）：清理句柄 + 观察 panic。
            reap_handlers(&mut handlers);

            tokio::select! {
                maybe_msg = reader.next::<InboundMessage>() => {
                    match maybe_msg {
                        Ok(Some(msg)) => match classify_inbound(&msg) {
                            Ok(InboundKind::Request { id, method, params }) => {
                                spawn_request(&mut handlers, &agent, &client, id, method, params);
                            }
                            Ok(InboundKind::Notification { method, params }) => {
                                spawn_notification(&mut handlers, &agent, &client, method, params);
                            }
                            Ok(InboundKind::Response { id, result }) => {
                                // 对 agent 请求的应答：路由到 pending（完整 JSON-RPC id）。
                                client.resolve_pending(id, result).await;
                            }
                            Err((error_id, code, message)) => {
                                // 非法消息：回标准 JSON-RPC 错误（id 为原合法 id 或 null）。
                                let resp = OutboundMessage::error(error_id, code, message);
                                let _ = client.send_outbound(resp);
                            }
                        },
                        Ok(None) => break Ok(()), // EOF
                        Err(e) => break Err(AcpError::JsonRpc(format!("read error: {e}"))),
                    }
                }
                maybe_err = writer_error_rx.recv() => {
                    // writer 写失败（Issue 1）：统一清理并返回 IO 错误。
                    // `None`（sender 已 drop 且无错误）表示 writer task 异常退出。
                    let err = match maybe_err {
                        Some(e) => e,
                        None => AcpError::JsonRpc("writer task exited unexpectedly".into()),
                    };
                    break Err(err);
                }
            }
        };

        // 统一清理（EOF / 读错误 / writer 错误都走这里）：
        // 取消 writer task + 取消全部 pending 请求 + abort 并回收 handler task。
        teardown(&shutdown, &client, &mut handlers, writer_task).await;
        result
    }
}

/// 回收已完成的 handler task（Issue 2/4）：清理句柄 + 观察 `JoinError`（panic 不静默）。
fn reap_handlers(handlers: &mut JoinSet<()>) {
    while let Some(res) = handlers.try_join_next() {
        if let Err(e) = res {
            if e.is_panic() {
                tracing::error!("acp stdio: handler task panicked: {e}");
            } else {
                tracing::warn!("acp stdio: handler task cancelled: {e}");
            }
        }
    }
}

/// spawn 一个请求 handler task（处理并回 JSON-RPC 应答）。
fn spawn_request(
    handlers: &mut JoinSet<()>,
    agent: &Arc<AcpAgent>,
    client: &Arc<StdioClient>,
    id: Value,
    method: String,
    params: Value,
) {
    let agent = Arc::clone(agent);
    let client = Arc::clone(client);
    handlers.spawn(async move {
        let result = agent.handle(&*client, &method, params).await;
        let resp = match result {
            Ok(value) => OutboundMessage::result(id, value),
            Err(e) => OutboundMessage::error(id, -32603, e.to_string()),
        };
        let _ = client.send_outbound(resp);
    });
}

/// spawn 一个 notification handler task（处理，不应答）。
fn spawn_notification(
    handlers: &mut JoinSet<()>,
    agent: &Arc<AcpAgent>,
    client: &Arc<StdioClient>,
    method: String,
    params: Value,
) {
    let agent = Arc::clone(agent);
    let client = Arc::clone(client);
    handlers.spawn(async move {
        let _ = agent.handle(&*client, &method, params).await;
    });
}

/// 连接清理（EOF / 读错误 / writer 错误统一走这里）：
/// 取消 writer task + 取消全部 pending 请求 + abort 并回收 handler task。
async fn teardown(
    shutdown: &CancellationToken,
    client: &Arc<StdioClient>,
    handlers: &mut JoinSet<()>,
    writer_task: tokio::task::JoinHandle<()>,
) {
    shutdown.cancel();
    client.cancel_all().await;
    handlers.abort_all();
    while handlers.join_next().await.is_some() {}
    let _ = writer_task.await;
}

#[cfg(feature = "acp-sse")]
impl AcpAgent {
    /// SSE+HTTP 传输（feature-gated，多 client）。
    ///
    /// 本任务降级为后续（stdio 为必做 DoD）；存根返回明确错误，不静默。
    pub async fn serve_sse(self, _addr: std::net::SocketAddr) -> Result<(), AcpError> {
        Err(AcpError::JsonRpc(
            "acp-sse transport is not implemented in this phase".into(),
        ))
    }
}
