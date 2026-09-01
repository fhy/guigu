//! RemoteClient：跨进程的 handle 等价物。
//!
//! 本地重建进程内契约：`watch` 缓存 snapshot、`broadcast` 重建事件源、
//! `pending` 关联请求/应答。命令面与 `AgentHandle`/`Agent` trait 同名。
//!
//! 并发模型：单一读 task（解析 `ServerMessage`）+ 单一写 task（`mpsc` 汇入 →
//! 写循环）。命令方法分配 `id`、插 oneshot 入 `pending`、`tx.send`、`await`
//! oneshot（30s 超时兜底）。`abort` 入队即返回，不等待应答。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot, watch};

use super::codec::{LineReader, write_line};
use super::protocol::{RemoteError, RemoteRequest, ServerMessage};
use crate::core::agent::AgentSnapshot;
use crate::core::event::AgentEvent;
use crate::core::message::{Message, ThinkingLevel};

/// 命令超时（默认 30s）。
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// 命令应答载荷（`Ok(())` 成功 / `Err(msg)` 服务端 `AgentError` 字符串化）。
type CommandReply = Result<(), String>;
/// 请求关联表：`id` → oneshot sender。
type PendingMap<T> = HashMap<u64, oneshot::Sender<T>>;

/// 远程客户端：跨进程的 handle 等价物。
pub struct RemoteClient {
    tx: mpsc::UnboundedSender<RemoteRequest>,
    snapshot: watch::Receiver<AgentSnapshot>,
    events: broadcast::Sender<AgentEvent>,
    /// 命令应答关联表（`Response` → oneshot）。
    pending: Arc<Mutex<PendingMap<CommandReply>>>,
    /// 快照应答关联表（`GetSnapshot` → oneshot）。
    pending_snapshots: Arc<Mutex<PendingMap<AgentSnapshot>>>,
    next_id: AtomicU64,
    closed: watch::Receiver<bool>,
}

impl RemoteClient {
    /// 连接既有字节流（stdio/tcp 由 connector 提供）。
    pub async fn connect<S>(stream: S) -> Result<Self, RemoteError>
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let (read_half, write_half) = tokio::io::split(stream);

        let (tx, mut rx) = mpsc::unbounded_channel::<RemoteRequest>();
        let (snapshot_tx, snapshot_rx) = watch::channel(initial_snapshot());
        let (events_tx, _events_rx) = broadcast::channel(100);
        let (closed_tx, closed_rx) = watch::channel(false);
        let pending: Arc<Mutex<PendingMap<CommandReply>>> = Arc::new(Mutex::new(HashMap::new()));
        let pending_snapshots: Arc<Mutex<PendingMap<AgentSnapshot>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // 单一读 task：循环解析 ServerMessage。
        {
            let mut reader = LineReader::new(read_half);
            let pending_clone = pending.clone();
            let pending_snapshots_clone = pending_snapshots.clone();
            let snapshot_tx_clone = snapshot_tx.clone();
            let events_tx_clone = events_tx.clone();
            let closed_tx_clone = closed_tx.clone();
            tokio::spawn(async move {
                loop {
                    match reader.next::<ServerMessage>().await {
                        Ok(Some(msg)) => match msg {
                            ServerMessage::Response { id, result } => {
                                if let Some(tx) = pending_clone.lock().await.remove(&id) {
                                    let _ = tx.send(result);
                                }
                            }
                            ServerMessage::Snapshot { id, snapshot } => {
                                // 更新本地 watch 缓存；非初始快照（id != 0）同时
                                // 结算对应的 GetSnapshot 请求。
                                let _ = snapshot_tx_clone.send(snapshot.clone());
                                if id != 0
                                    && let Some(tx) =
                                        pending_snapshots_clone.lock().await.remove(&id)
                                {
                                    let _ = tx.send(snapshot);
                                }
                            }
                            ServerMessage::Event { event } => {
                                let _ = events_tx_clone.send(event);
                            }
                        },
                        Ok(None) => break, // EOF
                        Err(e) => {
                            tracing::warn!("remote client: read error: {e}");
                            break;
                        }
                    }
                }
                // 连接关闭：标记 closed，并 drop 所有在途 oneshot sender，使
                // 等待中的命令 / 快照请求立即以 Protocol 错误返回（不等超时）。
                let _ = closed_tx_clone.send(true);
                pending_clone.lock().await.drain();
                pending_snapshots_clone.lock().await.drain();
            });
        }

        // 单一写 task：从 mpsc 取 RemoteRequest 写入写半。写失败时标记
        // closed 并排空 pending，使在途请求立即失败（不等 30s 超时）。
        {
            let mut write_half = write_half;
            let closed_tx_clone = closed_tx.clone();
            let pending_clone = pending.clone();
            let pending_snapshots_clone = pending_snapshots.clone();
            tokio::spawn(async move {
                let write_failed = loop {
                    match rx.recv().await {
                        Some(req) => {
                            if let Err(e) = write_line(&mut write_half, &req).await {
                                tracing::warn!("remote client: write error: {e}");
                                break true;
                            }
                        }
                        None => break false, // channel closed
                    }
                };
                if write_failed {
                    // 写失败：标记 closed，排空 pending 与 pending_snapshots，
                    // 使等待中的命令 / 快照请求立即以 Protocol 错误返回。
                    let _ = closed_tx_clone.send(true);
                    pending_clone.lock().await.drain();
                    pending_snapshots_clone.lock().await.drain();
                }
            });
        }

        Ok(Self {
            tx,
            snapshot: snapshot_rx,
            events: events_tx,
            pending,
            pending_snapshots,
            next_id: AtomicU64::new(1),
            closed: closed_rx,
        })
    }

    /// 分配下一个请求 id（单调递增，从 1 开始；0 保留给初始 Snapshot）。
    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// 发送一条命令并等待应答（带 30s 超时兜底）。
    async fn send_command(&self, req: RemoteRequest) -> Result<(), RemoteError> {
        if *self.closed.borrow() {
            return Err(RemoteError::Protocol("connection closed".into()));
        }
        let id = req.id();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        if self.tx.send(req).is_err() {
            self.pending.lock().await.remove(&id);
            return Err(RemoteError::Protocol("write channel closed".into()));
        }
        // 入队后再次检查关闭状态（连接可能在入队前关闭，写 task 已排空
        // pending，使本请求立即失败）。
        if *self.closed.borrow() {
            self.pending.lock().await.remove(&id);
            return Err(RemoteError::Protocol("connection closed".into()));
        }
        match tokio::time::timeout(COMMAND_TIMEOUT, rx).await {
            Ok(Ok(result)) => result.map_err(RemoteError::Command),
            Ok(Err(_)) => Err(RemoteError::Protocol("response channel closed".into())),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(RemoteError::Timeout)
            }
        }
    }

    /// 发送提示消息（入队即返回，不等待 run 结束）。
    pub async fn prompt(&self, messages: Vec<Message>) -> Result<(), RemoteError> {
        let id = self.next_id();
        self.send_command(RemoteRequest::Prompt { id, messages })
            .await
    }

    /// 继续处理（入队即返回）。
    pub async fn continue_(&self) -> Result<(), RemoteError> {
        let id = self.next_id();
        self.send_command(RemoteRequest::Continue { id }).await
    }

    /// 转向指定消息（入队即返回）。
    pub async fn steer(&self, message: Message) -> Result<(), RemoteError> {
        let id = self.next_id();
        self.send_command(RemoteRequest::Steer { id, message })
            .await
    }

    /// 跟进指定消息（入队即返回）。
    pub async fn follow_up(&self, message: Message) -> Result<(), RemoteError> {
        let id = self.next_id();
        self.send_command(RemoteRequest::FollowUp { id, message })
            .await
    }

    /// 中止当前操作（入队即返回，不等待应答）。
    ///
    /// 入队前后均检查 `closed`：写 task 失败后会标记 `closed` 并排空 pending，
    /// 但其 `rx` 在 task 退出前仍存活，`tx.send` 可能成功入队却无人消费。
    /// 一旦观察到 `closed=true` 必须返回 `Err`，不得返回 `Ok`。
    pub fn abort(&self) -> Result<(), RemoteError> {
        if *self.closed.borrow() {
            return Err(RemoteError::Protocol("connection closed".into()));
        }
        let id = self.next_id();
        if self.tx.send(RemoteRequest::Abort { id }).is_err() {
            return Err(RemoteError::Protocol("write channel closed".into()));
        }
        // 入队后再次检查关闭状态（写 task 可能在入队前已失败并标记 closed，
        // 此时该请求无人消费，不得返回 Ok）。
        if *self.closed.borrow() {
            return Err(RemoteError::Protocol("connection closed".into()));
        }
        Ok(())
    }

    /// 重置 agent（清空 transcript 与队列，入队即返回）。
    pub async fn reset(&self) -> Result<(), RemoteError> {
        let id = self.next_id();
        self.send_command(RemoteRequest::Reset { id }).await
    }

    /// 请求当前快照（服务端回 `Snapshot`，本方法阻塞至应答并返回该快照）。
    ///
    /// 同时更新本地 watch 缓存（`snapshot()` 可读）。
    pub async fn get_snapshot(&self) -> Result<AgentSnapshot, RemoteError> {
        if *self.closed.borrow() {
            return Err(RemoteError::Protocol("connection closed".into()));
        }
        let id = self.next_id();
        let (tx, rx) = oneshot::channel();
        self.pending_snapshots.lock().await.insert(id, tx);
        if self.tx.send(RemoteRequest::GetSnapshot { id }).is_err() {
            self.pending_snapshots.lock().await.remove(&id);
            return Err(RemoteError::Protocol("write channel closed".into()));
        }
        // 入队后再次检查关闭状态（连接可能在入队前关闭）。
        if *self.closed.borrow() {
            self.pending_snapshots.lock().await.remove(&id);
            return Err(RemoteError::Protocol("connection closed".into()));
        }
        match tokio::time::timeout(COMMAND_TIMEOUT, rx).await {
            Ok(Ok(snapshot)) => Ok(snapshot),
            Ok(Err(_)) => Err(RemoteError::Protocol("response channel closed".into())),
            Err(_) => {
                self.pending_snapshots.lock().await.remove(&id);
                Err(RemoteError::Timeout)
            }
        }
    }

    /// 关闭 agent（发 Shutdown 并等待应答）。
    pub async fn shutdown(self) -> Result<(), RemoteError> {
        let id = self.next_id();
        self.send_command(RemoteRequest::Shutdown { id }).await
    }

    /// 获取当前 agent 快照（本地 watch 最新）。
    pub fn snapshot(&self) -> AgentSnapshot {
        self.snapshot.borrow().clone()
    }

    /// 订阅 agent 事件（本地重建事件源）。
    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.events.subscribe()
    }
}

/// 初始快照（连接建立前的占位值，立即被服务端初始 Snapshot 覆盖）。
fn initial_snapshot() -> AgentSnapshot {
    AgentSnapshot {
        system_prompt: String::new(),
        model: None,
        thinking_level: ThinkingLevel::Off,
        messages: Vec::new(),
        is_streaming: false,
        streaming_message: None,
        pending_tool_calls: std::collections::HashSet::new(),
        error_message: None,
    }
}

#[cfg(test)]
mod tests;
