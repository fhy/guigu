//! ACP SSE+HTTP 传输（Task 025）：JSON-RPC 2.0 over SSE + HTTP（多 client）。
//!
//! ACP v1 官方 spec 未定稿 SSE transport（仅 stdio），故本模块采用任务规格约定的
//! **最小 wire 约定**（端点 / 字段 / 错误码以本 doc 为权威）：
//!
//! - `GET /sse`：建立 agent→client 事件流。服务器生成 `client_id`（`AtomicU64`
//!   单调计数，不新增 `uuid` crate），注册进 `ClientRegistry`，**首个 SSE event**
//!   回 `event: client_id` / `data: {"client_id": "..."}`（供后续 `POST /message`
//!   定位 client）。此后 agent→client 的 request / notification 均从该流推出。
//! - `POST /message`：client→agent。body 为 JSON-RPC 2.0 消息（request /
//!   notification / response），`client_id` 经 `X-Client-Id` header 携带。dispatch
//!   到 `AcpAgent::handle`；若 body 是对某 pending request 的应答（`id` 命中
//!   pending map）则 resolve。
//!
//! SSE event 格式：`event: <name>`、`data: <JSON-RPC payload>`。
//! - `event: client_id`：握手（首个 event，`data` 为 `{"client_id": "..."}`）。
//! - `event: <method>`：agent→client request（带 `id`，需 client 应答）或
//!   notification（无 `id`，如 `session/update`）。
//! - `event: response`：agent→client 对 client request 的应答（带 `id`）。
//!
//! client 解析 `data` 为 JSON-RPC 消息，按 `method` / `id` / `result` / `error`
//! 的有无判定种类（request / notification / response），`event:` 字段仅作快速过滤。
//!
//! 多 client 模型：每个 SSE 连接 = 一个 client，注册进 `ClientRegistry`
//! （`client_id → Arc<SseAcpClient>`）。多 client 共享同一 `AgentServer` 后端
//! （`AcpAgent` 包 `Arc` 经 axum `State` 注入）。连接断开（SSE 流 close → mpsc
//! receiver drop）→ watchdog 经 `Sender::closed()` 感知 → `ClientRegistry` 移除
//! 对应 `client_id` + 清空其 pending（无泄漏）。
//!
//! 双工：`SseAcpClient::notify` 经 mpsc 推 SSE；`SseAcpClient::request` 分配
//! per-client request id → 注册 pending（`(client_id, id) → oneshot`）→ 经 mpsc
//! 推带 id 的 request event → `await oneshot`（`tokio::time::timeout` 兜底，
//! `Elapsed` 映射 `AcpError::Io(TimedOut)`，不新增 `AcpError` 变体）。
//!
//! 边界声明（同任务规格）：一期不做 authenticate / TLS / 断线重连续流 / 多 client
//! 共享同一 session 广播；HTTP 用 axum 默认行为 + 有界 mpsc（满则 `notify` /
//! `request` 报错）。

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::extract::State;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::stream::{self, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc};
use tokio_stream::wrappers::ReceiverStream;

use crate::acp::jsonrpc::{
    InboundKind, InboundMessage, OutboundMessage, RequestId, classify_inbound,
};
use crate::acp::sse_client::{ClientId, PendingMap, SseAcpClient, SseEvent};
use crate::acp::{AcpAgent, AcpError};

/// agent→client request（`fs/*` / `session/request_permission`）默认超时。
const DEFAULT_SSE_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// SSE 事件通道容量（有界；满则 `notify` / `request` 报错，见边界声明）。
const SSE_CHANNEL_CAPACITY: usize = 256;

/// SSE 传输状态（经 axum `State` 共享给所有连接 handler）。
pub struct SseTransport {
    /// 多 client 共享的 `AgentServer` 后端（013）。
    agent: Arc<AcpAgent>,
    /// client 注册表：`client_id` → client 句柄。
    clients: Arc<Mutex<HashMap<ClientId, Arc<SseAcpClient>>>>,
    /// pending 请求表（所有 client 共享）。
    pending: Arc<Mutex<PendingMap>>,
    /// `client_id` 分配器（单调递增）。
    next_client_id: AtomicU64,
}

impl SseTransport {
    /// 创建 SSE 传输状态（绑定共享 `AcpAgent`）。
    pub fn new(agent: Arc<AcpAgent>) -> Self {
        Self {
            agent,
            clients: Arc::new(Mutex::new(HashMap::new())),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_client_id: AtomicU64::new(1),
        }
    }

    /// 分配一个单调递增的 `client_id`。
    pub fn allocate_client_id(&self) -> ClientId {
        self.next_client_id
            .fetch_add(1, Ordering::SeqCst)
            .to_string()
    }

    /// 注册一个 client（SSE 连接建立时调用）。
    pub async fn register_client(&self, id: &str, client: Arc<SseAcpClient>) {
        self.clients.lock().await.insert(id.to_string(), client);
    }

    /// 取一个 client 句柄（`POST /message` dispatch 时调用）；不存在返回 `None`。
    pub async fn get_client(&self, id: &str) -> Option<Arc<SseAcpClient>> {
        self.clients.lock().await.get(id).cloned()
    }

    /// 移除一个 client（断连时调用）：从注册表移除 + 清空其全部 pending（以明确
    /// 错误结束，使等待中的 `request` 立即返回而非永久挂起）。
    pub async fn remove_client(&self, id: &str) {
        self.clients.lock().await.remove(id);
        let mut pending = self.pending.lock().await;
        // 先收集该 client 的 pending key，再逐个 `remove` 取 oneshot 所有权并
        // 以明确错误结束（oneshot `Sender` 不可 `Clone`，须经 `remove` 消费）。
        let keys: Vec<(ClientId, RequestId)> = pending
            .keys()
            .filter(|(cid, _)| cid.as_str() == id)
            .cloned()
            .collect();
        for key in keys {
            if let Some(tx) = pending.remove(&key) {
                let _ = tx.send(Err(AcpError::JsonRpc("connection closed".into())));
            }
        }
    }

    /// 当前注册 client 数（单测断言注册表清理用）。
    #[cfg(test)]
    pub(crate) async fn client_count(&self) -> usize {
        self.clients.lock().await.len()
    }

    /// 路由一条 client 应答到 pending 请求（`POST /message` 收到应答时调用）。
    pub async fn resolve_pending(
        &self,
        client_id: &str,
        id: RequestId,
        result: Result<Value, AcpError>,
    ) {
        let mut pending = self.pending.lock().await;
        if let Some(tx) = pending.remove(&(client_id.to_string(), id)) {
            let _ = tx.send(result);
        }
    }

    /// 测试用：取共享 pending 表句柄（供单测构造共享同一 pending 的 `SseAcpClient`）。
    #[cfg(test)]
    pub(crate) fn pending_for_test(&self) -> Arc<Mutex<PendingMap>> {
        Arc::clone(&self.pending)
    }
}

/// `GET /sse` handler：建立 agent→client 事件流。
///
/// 生成 `client_id` → 建 mpsc 通道 → 注册 client → spawn watchdog（断连清理）→
/// 返回 SSE 流（首个 event 回 `client_id`，此后转发 mpsc 事件）。
async fn sse_handler(State(state): State<Arc<SseTransport>>) -> Response {
    let client_id = state.allocate_client_id();
    let (tx, rx) = mpsc::channel::<SseEvent>(SSE_CHANNEL_CAPACITY);
    let client = Arc::new(SseAcpClient::new(
        client_id.clone(),
        tx.clone(),
        Arc::clone(&state.pending),
        DEFAULT_SSE_REQUEST_TIMEOUT,
    ));
    state.register_client(&client_id, Arc::clone(&client)).await;

    // watchdog：client 断开（SSE 流 drop → mpsc receiver drop → 通道 closed）时，
    // 清理注册表 + 清空该 client 的 pending（无泄漏）。
    {
        let tx_watch = tx.clone();
        let state_clone = Arc::clone(&state);
        let client_id_clone = client_id.clone();
        tokio::spawn(async move {
            tx_watch.closed().await;
            state_clone.remove_client(&client_id_clone).await;
        });
    }

    // 首个 SSE event 回 client_id（供后续 POST /message 定位 client）。
    let first: Result<Event, Infallible> = Ok(Event::default()
        .event("client_id")
        .data(json!({ "client_id": client_id }).to_string()));
    let rx_stream = ReceiverStream::new(rx)
        .map(|e| Ok::<Event, Infallible>(Event::default().event(e.event).data(e.data)));
    let stream = stream::once(async move { first }).chain(rx_stream);
    Sse::new(stream).into_response()
}

/// `POST /message` handler：client→agent JSON-RPC。
///
/// `client_id` 经 `X-Client-Id` header 携带。按 `classify_inbound` 分类：
/// - 应答（无 `method`、有 `id`）：路由到 pending，回 `204`。
/// - 请求 / notification：spawn 独立 handler task 调 `AcpAgent::handle`（请求的
///   应答经 SSE `event: response` 回推），回 `202`。
/// - 非法消息：回 `400` + JSON-RPC 错误。
async fn message_handler(
    State(state): State<Arc<SseTransport>>,
    headers: axum::http::HeaderMap,
    Json(msg): Json<InboundMessage>,
) -> impl IntoResponse {
    let client_id = match headers
        .get("x-client-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
    {
        Some(id) => id,
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({ "error": "missing X-Client-Id header" })),
            )
                .into_response();
        }
    };

    match classify_inbound(&msg) {
        Ok(InboundKind::Response { id, result }) => {
            state.resolve_pending(&client_id, id, result).await;
            axum::http::StatusCode::NO_CONTENT.into_response()
        }
        Ok(InboundKind::Request { id, method, params }) => {
            let client = match state.get_client(&client_id).await {
                Some(c) => c,
                None => {
                    return (
                        axum::http::StatusCode::NOT_FOUND,
                        Json(json!({ "error": "unknown client_id" })),
                    )
                        .into_response();
                }
            };
            // spawn 独立 task：`session/prompt`（阻塞至 run 结束）不阻塞 HTTP handler。
            let agent = Arc::clone(&state.agent);
            let client_clone = Arc::clone(&client);
            tokio::spawn(async move {
                let result = agent.handle(&*client_clone, &method, params).await;
                let resp = match result {
                    Ok(value) => OutboundMessage::result(id, value),
                    Err(e) => OutboundMessage::error(id, -32603, e.to_string()),
                };
                let _ = client_clone.send_response(&resp).await;
            });
            axum::http::StatusCode::ACCEPTED.into_response()
        }
        Ok(InboundKind::Notification { method, params }) => {
            let client = match state.get_client(&client_id).await {
                Some(c) => c,
                None => {
                    return (
                        axum::http::StatusCode::NOT_FOUND,
                        Json(json!({ "error": "unknown client_id" })),
                    )
                        .into_response();
                }
            };
            let agent = Arc::clone(&state.agent);
            let client_clone = Arc::clone(&client);
            tokio::spawn(async move {
                let _ = agent.handle(&*client_clone, &method, params).await;
            });
            axum::http::StatusCode::ACCEPTED.into_response()
        }
        Err((error_id, code, message)) => {
            // 非法消息：回标准 JSON-RPC 错误（作为 HTTP 响应体）。
            let resp = OutboundMessage::error(error_id, code, message);
            (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::to_value(&resp).unwrap_or(Value::Null)),
            )
                .into_response()
        }
    }
}

impl AcpAgent {
    /// SSE+HTTP 传输（feature-gated，多 client）。
    ///
    /// 起 axum HTTP server：`GET /sse`（agent→client 事件流）+ `POST /message`
    /// （client→agent JSON-RPC）。每个 SSE 连接 = 一个 client，注册进
    /// `ClientRegistry`；多 client 共享同一 `AgentServer` 后端。绑定失败 / server
    /// 退出时返回 `AcpError`。
    pub async fn serve_sse(self, addr: SocketAddr) -> Result<(), AcpError> {
        let transport = Arc::new(SseTransport::new(Arc::new(self)));
        let app = Router::new()
            .route("/sse", get(sse_handler))
            .route("/message", post(message_handler))
            .with_state(transport);
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app)
            .await
            .map_err(|e| AcpError::Io(std::io::Error::other(e.to_string())))
    }
}
