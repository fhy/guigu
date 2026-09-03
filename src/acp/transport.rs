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

/// JSON-RPC 请求 id（string 或 number）。
///
/// JSON-RPC 2.0 允许 id 为 string / number / null。guigu 只接受 string / number
/// 作为 pending key（`null` 仅用于错误应答，不作为 key）。number 用 `i64` 承载
/// （含负数），使合法的字符串 id / 负数 id 都能正确路由，而非被静默忽略。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RequestId {
    /// 数字 id（含负数）。
    Number(i64),
    /// 字符串 id。
    String(String),
}

impl RequestId {
    /// 从 JSON-RPC `id` 值解析（仅 string / number 合法；其余 → `None`）。
    pub fn from_value(v: &Value) -> Option<Self> {
        if let Some(n) = v.as_i64() {
            Some(RequestId::Number(n))
        } else {
            v.as_str().map(|s| RequestId::String(s.to_string()))
        }
    }

    /// 转回 JSON-RPC `id` 值（用于出站请求 / 应答）。
    pub fn to_value(&self) -> Value {
        match self {
            RequestId::Number(n) => Value::from(*n),
            RequestId::String(s) => Value::String(s.clone()),
        }
    }
}

/// 入站 JSON-RPC 消息（client→agent）：请求 / notification / 应答。
///
/// 有 `method` → 请求（有 `id`）或 notification（无 `id`）；无 `method` 且有 `id`
/// → 对 agent 请求的应答。
#[derive(Debug, Clone, Deserialize)]
pub struct InboundMessage {
    /// JSON-RPC 版本（须为 `"2.0"`，由 `classify_inbound` 校验）。
    #[serde(default)]
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
    pub fn request(id: &RequestId, method: &str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id.to_value()),
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

/// pending 请求表：完整 JSON-RPC id（string / number）→ 应答 oneshot。
type PendingMap = HashMap<RequestId, oneshot::Sender<Result<Value, AcpError>>>;

/// stdio 传输的 `AcpClient` 实现：经 writer 通道向 client 发请求 / notification，
/// pending 请求由读循环的应答路由唤醒。
pub struct StdioClient {
    /// 写通道（汇入 writer task）。
    writer: mpsc::UnboundedSender<OutboundMessage>,
    /// pending 请求：完整 JSON-RPC id → 应答 oneshot。
    pending: Arc<Mutex<PendingMap>>,
    /// 请求 id 分配器（单调递增，agent 出站请求用正整数 id）。
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
    pub async fn resolve_pending(&self, id: RequestId, result: Result<Value, AcpError>) {
        let mut pending = self.pending.lock().await;
        if let Some(tx) = pending.remove(&id) {
            let _ = tx.send(result);
        }
    }

    /// 取消全部 pending 请求（连接断开时调用）：以明确错误结束所有 oneshot 并清空表，
    /// 使等待中的 `request` 立即返回而非永久挂起（Issue 5）。
    pub async fn cancel_all(&self) {
        let mut pending = self.pending.lock().await;
        for (_, tx) in pending.drain() {
            let _ = tx.send(Err(AcpError::JsonRpc("connection closed".into())));
        }
    }

    /// pending 请求数（测试 / 诊断用）。
    pub async fn pending_len(&self) -> usize {
        self.pending.lock().await.len()
    }

    /// 测试用：插入一个 pending entry（任意 id），返回其 oneshot receiver。
    ///
    /// 用于验证字符串 / 负数 id 的应答路由（`request` 只分配正整数 id，无法覆盖
    /// 这些路径）。
    #[cfg(test)]
    pub async fn insert_pending_for_test(
        &self,
        id: RequestId,
    ) -> oneshot::Receiver<Result<Value, AcpError>> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        rx
    }
}

#[async_trait]
impl AcpClient for StdioClient {
    async fn request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let key = RequestId::Number(id as i64);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(key.clone(), tx);
        let msg = OutboundMessage::request(&key, method, params);
        // 发送失败（writer 已关闭）：移除 pending entry，避免 oneshot 泄漏（Issue 4）。
        if self.writer.send(msg).is_err() {
            self.pending.lock().await.remove(&key);
            return Err(AcpError::JsonRpc("writer closed".into()));
        }
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

/// 校验后的入站消息分类。
#[derive(Debug)]
pub(crate) enum InboundKind {
    /// 请求（有 `method` + `id`）：spawn 处理并回 JSON-RPC 应答。
    Request {
        /// 关联号（原样回显）。
        id: Value,
        /// 方法名。
        method: String,
        /// 请求参数。
        params: Value,
    },
    /// notification（有 `method`，无 `id`）：spawn 处理，不应答。
    Notification {
        /// 方法名。
        method: String,
        /// 请求参数。
        params: Value,
    },
    /// 应答（无 `method`，有合法 `id`）：路由到 pending 请求。
    Response {
        /// 关联号（完整 JSON-RPC id）。
        id: RequestId,
        /// 应答结果（`Ok` = result，`Err` = error）。
        result: Result<Value, AcpError>,
    },
}

/// 校验并分类一条入站 JSON-RPC 2.0 消息（建议 1）。
///
/// 返回 `Ok(kind)` 表示合法；`Err((id, code, message))` 表示非法，调用方应回
/// `OutboundMessage::error(id, code, message)`（`id` 为原消息的合法 id 或 `null`）。
///
/// 校验规则：
/// - `jsonrpc` 若存在须为 `"2.0"`；
/// - 含 `method` 时不得同时含 `result` / `error`；
/// - 无 `method`（应答）时须有合法 `id` 且恰含 `result` / `error` 之一。
pub(crate) fn classify_inbound(msg: &InboundMessage) -> Result<InboundKind, (Value, i64, String)> {
    // 1. 校验 jsonrpc 版本。
    if let Some(v) = &msg.jsonrpc
        && v != "2.0"
    {
        return Err((
            Value::Null,
            -32600,
            format!("invalid jsonrpc version: {v} (expected \"2.0\")"),
        ));
    }

    let has_result = msg.result.is_some();
    let has_error = msg.error.is_some();

    if let Some(method) = &msg.method {
        // 请求 / notification。
        if has_result || has_error {
            return Err((
                msg.id.clone().unwrap_or(Value::Null),
                -32600,
                "invalid request: both method and result/error present".into(),
            ));
        }
        let params = msg.params.clone().unwrap_or(Value::Null);
        match &msg.id {
            Some(id) => Ok(InboundKind::Request {
                id: id.clone(),
                method: method.clone(),
                params,
            }),
            None => Ok(InboundKind::Notification {
                method: method.clone(),
                params,
            }),
        }
    } else {
        // 应答：须有合法 id + 恰含 result / error 之一。
        if has_result && has_error {
            return Err((
                msg.id.clone().unwrap_or(Value::Null),
                -32600,
                "invalid response: both result and error present".into(),
            ));
        }
        if !has_result && !has_error {
            return Err((
                msg.id.clone().unwrap_or(Value::Null),
                -32600,
                "invalid response: missing result and error".into(),
            ));
        }
        let id = msg
            .id
            .as_ref()
            .and_then(RequestId::from_value)
            .ok_or_else(|| {
                (
                    Value::Null,
                    -32600,
                    "invalid response: missing or invalid id".to_string(),
                )
            })?;
        let result = match (&msg.result, &msg.error) {
            (Some(result), None) => Ok(result.clone()),
            (None, Some(error)) => Err(AcpError::JsonRpc(error.message.clone())),
            _ => unreachable!("checked above"),
        };
        Ok(InboundKind::Response { id, result })
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
        // in-flight handler task（请求 / notification 各 spawn 一个）；连接断开时统一
        // abort + 回收，避免 `session/prompt` 等长任务在 client 断开后永久挂起（Issue 5）。
        let mut handler_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();

        // 读循环：逐条解析入站消息。
        loop {
            match reader.next::<InboundMessage>().await {
                Ok(Some(msg)) => match classify_inbound(&msg) {
                    Ok(InboundKind::Request { id, method, params }) => {
                        // 请求：spawn 独立 task 处理并回 JSON-RPC 应答（双工）。
                        let agent = Arc::clone(&agent);
                        let client = Arc::clone(&client);
                        let tx = tx.clone();
                        let handle = tokio::spawn(async move {
                            let result = agent.handle(&*client, &method, params).await;
                            let resp = match result {
                                Ok(value) => OutboundMessage::result(id, value),
                                Err(e) => OutboundMessage::error(id, -32603, e.to_string()),
                            };
                            let _ = tx.send(resp);
                        });
                        handler_tasks.push(handle);
                    }
                    Ok(InboundKind::Notification { method, params }) => {
                        // notification：spawn 独立 task 处理，不应答。
                        let agent = Arc::clone(&agent);
                        let client = Arc::clone(&client);
                        let handle = tokio::spawn(async move {
                            let _ = agent.handle(&*client, &method, params).await;
                        });
                        handler_tasks.push(handle);
                    }
                    Ok(InboundKind::Response { id, result }) => {
                        // 对 agent 请求的应答：路由到 pending（完整 JSON-RPC id）。
                        client.resolve_pending(id, result).await;
                    }
                    Err((error_id, code, message)) => {
                        // 非法消息：回标准 JSON-RPC 错误（id 为原合法 id 或 null）。
                        let resp = OutboundMessage::error(error_id, code, message);
                        let _ = tx.send(resp);
                    }
                },
                Ok(None) => break, // EOF
                Err(e) => {
                    // 读错误：取消连接，清理 pending + handler task + writer task。
                    shutdown.cancel();
                    client.cancel_all().await;
                    let tasks = std::mem::take(&mut handler_tasks);
                    for task in &tasks {
                        task.abort();
                    }
                    for task in tasks {
                        let _ = task.await;
                    }
                    let _ = writer_task.await;
                    return Err(AcpError::JsonRpc(e.to_string()));
                }
            }
        }

        // EOF：取消连接（writer task 退出 + pending 请求以错误结束 + handler task 回收）。
        shutdown.cancel();
        client.cancel_all().await;
        for task in &handler_tasks {
            task.abort();
        }
        for task in handler_tasks {
            let _ = task.await;
        }
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
