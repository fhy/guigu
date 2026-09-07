//! SSE 版 `AcpClient` 实现（Task 025，feature `acp-sse`）。
//!
//! 从 `transport_sse` 拆出（单文件 ≤ 400 行约束，对齐 `stdio_client` 模式）。含：
//! - `ClientId` / `SseEvent` / `PendingMap`：wire 类型。
//! - `SseAcpClient`：SSE 版 `AcpClient`（`notify` 经 mpsc 推 SSE；`request` 注册
//!   pending + 经 mpsc 推带 id 的 request event + `await oneshot`（超时兜底，
//!   `Elapsed` 映射 `AcpError::Io(TimedOut)`，不新增 `AcpError` 变体））。
//!
//! per-client id 空间（`next_id` 独立计数）；pending 按 `(client_id, id)` 键控
//! （防跨 client 应答注入 + 断连按 client 批量清理）。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::acp::jsonrpc::{OutboundMessage, RequestId};
use crate::acp::{AcpClient, AcpError};

/// client 标识（`AtomicU64` 单调计数生成，不新增 `uuid` crate）。
pub type ClientId = String;

/// SSE 事件（mpsc 通道负载）：`event:` 字段 + `data:` 字段。
#[derive(Debug, Clone)]
pub struct SseEvent {
    /// SSE `event:` 字段（方法名 / `client_id` / `response`）。
    pub event: String,
    /// SSE `data:` 字段（JSON-RPC payload，序列化字符串）。
    pub data: String,
}

/// pending 请求表：`(client_id, JSON-RPC id)` → 应答 oneshot。
///
/// 按 `(client_id, id)` 键控：每个 client 是独立 JSON-RPC peer（自有 id 空间），
/// 按 `client_id` 作用域可防跨 client 应答注入，并支持断连时按 client 批量清理。
pub type PendingMap = HashMap<(ClientId, RequestId), oneshot::Sender<Result<Value, AcpError>>>;

/// SSE 版 `AcpClient` 实现（每个 SSE 连接一个）。
///
/// `notify` 经 mpsc 推 SSE；`request` 注册 pending + 经 mpsc 推带 id 的 request
/// event，再 `await oneshot`（超时兜底）。per-client id 空间（`next_id` 独立计数）。
pub struct SseAcpClient {
    /// 本连接所属 client。
    client_id: ClientId,
    /// SSE 事件发送端（接收端由 SSE 响应流持有）。
    tx: mpsc::Sender<SseEvent>,
    /// pending 请求表（与 transport 共享）。
    pending: Arc<Mutex<PendingMap>>,
    /// 请求 id 分配器（per-client 单调递增）。
    next_id: AtomicU64,
    /// agent→client request 超时。
    timeout: Duration,
}

impl SseAcpClient {
    /// 创建 SSE client（绑定 `client_id` / 发送端 / 共享 pending / 超时）。
    pub fn new(
        client_id: ClientId,
        tx: mpsc::Sender<SseEvent>,
        pending: Arc<Mutex<PendingMap>>,
        timeout: Duration,
    ) -> Self {
        Self {
            client_id,
            tx,
            pending,
            next_id: AtomicU64::new(1),
            timeout,
        }
    }

    /// 测试用：取共享 pending 表句柄（供单测直接 resolve / 断言 pending）。
    #[cfg(test)]
    pub(crate) fn pending_for_test(&self) -> Arc<Mutex<PendingMap>> {
        Arc::clone(&self.pending)
    }

    /// 经 SSE 推一条 JSON-RPC 应答（对 client request 的应答，`event: response`）。
    pub(crate) async fn send_response(&self, resp: &OutboundMessage) -> Result<(), AcpError> {
        let data = serde_json::to_string(resp)?;
        self.tx
            .send(SseEvent {
                event: "response".into(),
                data,
            })
            .await
            .map_err(|_| AcpError::JsonRpc("client disconnected".into()))
    }
}

#[async_trait]
impl AcpClient for SseAcpClient {
    async fn request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        // 无损转换：`u64` 超出 `i64::MAX` 时返回明确错误（对齐 stdio，避免回绕冲突）。
        let key_id = match i64::try_from(id) {
            Ok(n) => RequestId::Number(n),
            Err(_) => {
                return Err(AcpError::JsonRpc(
                    "request id overflow: too many in-flight requests".into(),
                ));
            }
        };
        let key = (self.client_id.clone(), key_id.clone());
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(key, tx);
        let msg = OutboundMessage::request(&key_id, method, params);
        let data = serde_json::to_string(&msg)?;
        // 发送失败（client 已断开）：移除 pending entry，避免 oneshot 泄漏。
        if self
            .tx
            .send(SseEvent {
                event: method.to_string(),
                data,
            })
            .await
            .is_err()
        {
            self.pending
                .lock()
                .await
                .remove(&(self.client_id.clone(), key_id));
            return Err(AcpError::JsonRpc("client disconnected".into()));
        }
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(AcpError::JsonRpc("request cancelled".into())),
            Err(_) => {
                // 超时：移除 pending entry，避免 oneshot 泄漏。
                self.pending
                    .lock()
                    .await
                    .remove(&(self.client_id.clone(), key_id));
                Err(AcpError::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "agent→client request timed out",
                )))
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), AcpError> {
        let msg = OutboundMessage::notification(method, params);
        let data = serde_json::to_string(&msg)?;
        self.tx
            .send(SseEvent {
                event: method.to_string(),
                data,
            })
            .await
            .map_err(|_| AcpError::JsonRpc("client disconnected".into()))
    }
}
