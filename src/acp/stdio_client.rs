//! stdio 传输的 `AcpClient` 实现与连接资源（Task 014）。
//!
//! 从 `transport.rs` 拆出（单文件 ≤ 400 行约束）。含：
//! - `StdioClient`：经 writer 通道向 client 发请求 / notification，pending 请求
//!   由读循环的应答路由唤醒。
//! - `StdioConnection`：连接资源（`StdioClient` + writer 接收端），由
//!   `serve_connection` 内部创建，或经 `serve_connection_with` 由调用方注入。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::acp::jsonrpc::{OutboundMessage, RequestId};
use crate::acp::{AcpClient, AcpError};

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
    /// 使等待中的 `request` 立即返回而非永久挂起。
    pub async fn cancel_all(&self) {
        let mut pending = self.pending.lock().await;
        for (_, tx) in pending.drain() {
            let _ = tx.send(Err(AcpError::JsonRpc("connection closed".into())));
        }
    }

    /// 发送一条出站消息（应答 / 错误应答 / notification）到 writer 通道。
    ///
    /// 读循环（非法消息回错误应答）与 handler task（回请求应答）共用此入口，
    /// 避免各自持有写通道发送端。
    pub fn send_outbound(&self, msg: OutboundMessage) -> Result<(), AcpError> {
        self.writer
            .send(msg)
            .map_err(|_| AcpError::JsonRpc("writer closed".into()))
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
        // 无损转换：`u64` 超出 `i64::MAX` 时返回明确错误，而非 `as` 回绕造成
        // 请求 id 冲突（建议 2）。
        let key = match i64::try_from(id) {
            Ok(n) => RequestId::Number(n),
            Err(_) => {
                return Err(AcpError::JsonRpc(
                    "request id overflow: too many in-flight requests".into(),
                ));
            }
        };
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(key.clone(), tx);
        let msg = OutboundMessage::request(&key, method, params);
        // 发送失败（writer 已关闭）：移除 pending entry，避免 oneshot 泄漏。
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

/// stdio 连接资源：`StdioClient`（pending 请求 + 写通道发送端）+ writer 接收端。
///
/// 由 `serve_connection` 内部创建，或经 `serve_connection_with` 由调用方注入
/// （测试 / 嵌入方可注入 `StdioClient` 以驱动 agent→client 请求并观察 pending）。
pub struct StdioConnection {
    /// client 句柄（`Arc` 包装，供 handler / 工具克隆注入）。
    client: Arc<StdioClient>,
    /// writer task 的接收端。
    writer_rx: mpsc::UnboundedReceiver<OutboundMessage>,
}

impl StdioConnection {
    /// 创建连接资源（内部建 mpsc 写通道）。
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            client: Arc::new(StdioClient::new(tx)),
            writer_rx: rx,
        }
    }

    /// 取 `StdioClient` 句柄（`Arc` 克隆，供工具 / handler 注入）。
    pub fn client(&self) -> Arc<StdioClient> {
        Arc::clone(&self.client)
    }

    /// 拆出内部资源（`serve_connection_with` 调用）。
    pub(crate) fn into_parts(self) -> (Arc<StdioClient>, mpsc::UnboundedReceiver<OutboundMessage>) {
        (self.client, self.writer_rx)
    }
}

impl Default for StdioConnection {
    fn default() -> Self {
        Self::new()
    }
}
