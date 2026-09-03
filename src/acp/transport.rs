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
//!
//! SSE+HTTP 为可选加分项（`acp-sse` feature），本任务降级为后续（`serve_sse` 存根）。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::acp::{AcpAgent, AcpClient, AcpError};
use crate::remote::codec::{LineReader, write_line};

/// JSON-RPC 错误对象。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcErrorObj {
    /// 错误码（`-32603` = Internal error）。
    pub code: i64,
    /// 错误消息。
    pub message: String,
    /// 附加数据（可选）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// 入站 JSON-RPC 消息（client→agent）：请求 / notification / 应答。
///
/// 有 `method` → 请求（有 `id`）或 notification（无 `id`）；无 `method` 且有 `id`
/// → 对 agent 请求的应答。
#[derive(Debug, Clone, Deserialize)]
pub struct InboundMessage {
    /// JSON-RPC 版本（`"2.0"`）；保留字段以符合协议，一期不校验。
    #[serde(default)]
    #[allow(dead_code)]
    pub jsonrpc: Option<String>,
    /// 关联号；notification 缺省。
    pub id: Option<Value>,
    /// 方法名；应答缺省。
    pub method: Option<String>,
    /// 请求参数。
    pub params: Option<Value>,
    /// 应答结果。
    pub result: Option<Value>,
    /// 应答错误。
    pub error: Option<JsonRpcErrorObj>,
}

/// 出站 JSON-RPC 消息（agent→client）：请求 / notification / 应答。
#[derive(Debug, Clone, Serialize)]
pub struct OutboundMessage {
    pub jsonrpc: String,
    /// 关联号；notification 缺省。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// 方法名；应答缺省。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// 请求参数。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    /// 应答结果。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// 应答错误。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcErrorObj>,
}

impl OutboundMessage {
    /// 构造对 client 请求的应答（成功）。
    pub fn result(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id),
            method: None,
            params: None,
            result: Some(result),
            error: None,
        }
    }

    /// 构造对 client 请求的应答（错误）。
    pub fn error(id: Value, code: i64, message: String) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id),
            method: None,
            params: None,
            result: None,
            error: Some(JsonRpcErrorObj {
                code,
                message,
                data: None,
            }),
        }
    }

    /// 构造 agent→client 请求（期望应答）。
    pub fn request(id: u64, method: &str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(Value::from(id)),
            method: Some(method.into()),
            params: Some(params),
            result: None,
            error: None,
        }
    }

    /// 构造 agent→client notification（无应答）。
    pub fn notification(method: &str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: None,
            method: Some(method.into()),
            params: Some(params),
            result: None,
            error: None,
        }
    }
}

/// pending 请求表：id → 应答 oneshot。
type PendingMap = HashMap<u64, oneshot::Sender<Result<Value, AcpError>>>;

/// stdio 传输的 `AcpClient` 实现：经 writer 通道向 client 发请求 / notification，
/// pending 请求由读循环的应答路由唤醒。
pub struct StdioClient {
    /// 写通道（汇入 writer task）。
    writer: mpsc::UnboundedSender<OutboundMessage>,
    /// pending 请求：id → 应答 oneshot。
    pending: Arc<Mutex<PendingMap>>,
    /// 请求 id 分配器（单调递增）。
    next_id: AtomicU64,
}

impl StdioClient {
    /// 创建 stdio client（绑定写通道）。
    pub fn new(writer: mpsc::UnboundedSender<OutboundMessage>) -> Self {
        Self {
            writer,
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
        }
    }

    /// 路由一条 client 应答到 pending 请求（读循环调用）。
    pub async fn resolve_pending(&self, id: u64, result: Result<Value, AcpError>) {
        let mut pending = self.pending.lock().await;
        if let Some(tx) = pending.remove(&id) {
            let _ = tx.send(result);
        }
    }
}

#[async_trait]
impl AcpClient for StdioClient {
    async fn request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let msg = OutboundMessage::request(id, method, params);
        self.writer
            .send(msg)
            .map_err(|_| AcpError::JsonRpc("writer closed".into()))?;
        match rx.await {
            Ok(result) => result,
            Err(_) => Err(AcpError::JsonRpc("request cancelled".into())),
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), AcpError> {
        let msg = OutboundMessage::notification(method, params);
        self.writer
            .send(msg)
            .map_err(|_| AcpError::JsonRpc("writer closed".into()))
    }
}

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
        let mut reader = LineReader::new(reader);
        let mut writer = writer;

        // 写半归单一 writer task：mpsc 汇入 → 写循环。
        let (tx, mut rx) = mpsc::unbounded_channel::<OutboundMessage>();
        // EOF / 错误时显式通知 writer task 退出：`StdioClient` 持有 `tx` 克隆且
        // 生命周期与 `serve_connection` 相同，故 `rx.recv()` 不会因 sender 归零而
        // 返回 `None`，须用取消令牌兜底。
        let shutdown = CancellationToken::new();
        let writer_shutdown = shutdown.clone();
        let writer_task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    msg = rx.recv() => match msg {
                        Some(msg) => {
                            if let Err(e) = write_line(&mut writer, &msg).await {
                                tracing::warn!("acp stdio: write failed: {e}");
                                break;
                            }
                        }
                        None => break,
                    },
                    _ = writer_shutdown.cancelled() => break,
                }
            }
        });

        let client = Arc::new(StdioClient::new(tx.clone()));
        let agent = Arc::new(self);

        // 读循环：逐条解析入站消息。
        loop {
            match reader.next::<InboundMessage>().await {
                Ok(Some(msg)) => {
                    if let Some(method) = msg.method {
                        // 请求 / notification：spawn 独立 task 处理（双工）。
                        let id = msg.id.clone();
                        let params = msg.params.unwrap_or(Value::Null);
                        let agent = Arc::clone(&agent);
                        let client = Arc::clone(&client);
                        let tx = tx.clone();
                        tokio::spawn(async move {
                            let result = agent.handle(&*client, &method, params).await;
                            if let Some(id) = id {
                                let resp = match result {
                                    Ok(value) => OutboundMessage::result(id, value),
                                    Err(e) => OutboundMessage::error(id, -32603, e.to_string()),
                                };
                                let _ = tx.send(resp);
                            }
                        });
                    } else if let Some(id) = msg.id.as_ref().and_then(Value::as_u64) {
                        // 对 agent 请求的应答：路由到 pending。
                        let result = match (&msg.result, &msg.error) {
                            (Some(result), None) => Ok(result.clone()),
                            (None, Some(error)) => Err(AcpError::JsonRpc(error.message.clone())),
                            _ => Err(AcpError::JsonRpc("invalid response".into())),
                        };
                        client.resolve_pending(id, result).await;
                    }
                    // 既无 method 又无 id：畸形消息，忽略。
                }
                Ok(None) => break, // EOF
                Err(e) => {
                    shutdown.cancel();
                    let _ = writer_task.await;
                    return Err(AcpError::JsonRpc(e.to_string()));
                }
            }
        }

        // EOF：通知 writer task 退出（`StdioClient` 持有 `tx` 克隆，`rx.recv()`
        // 不会因 sender 归零返回 `None`），再等其收尾。
        shutdown.cancel();
        drop(tx);
        let _ = writer_task.await;
        Ok(())
    }
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
