//! 多连接 TCP server（Task 013）。
//!
//! - `serve_tcp`：监听 TCP，accept 循环；每连接 spawn 一个 task 跑 `serve_connection`。
//! - `serve_connection`：读 `ServerRequest` → 分发到 `AgentServer` 方法；订阅事件转发。
//!
//! 复用 010 codec（newline-delimited JSON）+ 写半单 writer task 模式（不持锁跨
//! await）。连接断开（EOF）只移除该连接的订阅，不关 session/lane（多 client 共享
//! session 语义）；`Shutdown` 请求则全局关闭所有 session/lane（协议语义）。
//! `SpawnLane` / `ForkLane` / `CreateSession` 的 runtime / storage 由服务端工厂构造
//! （wire 不携带不可序列化的 runtime / storage）。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;

use super::protocol::{ServerMessage, ServerRequest};
use super::{AgentServer, ServerError, SessionId, StorageFactory};
use crate::core::agent::AgentConfig;
use crate::core::event::AgentEvent;
use crate::core::runtime::AgentRuntime;
use crate::remote::codec::{LineReader, write_line};

/// 订阅条目：取消 token + forwarder task 句柄（cancel 后可 await 其退出）。
type Subscription = (CancellationToken, tokio::task::JoinHandle<()>);

impl AgentServer {
    /// 监听 TCP，accept 循环；每连接 spawn 一个 task 跑 `serve_connection`。
    pub async fn serve_tcp(self: Arc<Self>, addr: SocketAddr) -> Result<(), ServerError> {
        let listener = TcpListener::bind(addr).await?;
        loop {
            let (stream, _) = listener.accept().await?;
            let server = Arc::clone(&self);
            tokio::spawn(async move {
                let _ = server.serve_connection(stream).await;
            });
        }
    }

    /// 服务一条连接：读 `ServerRequest` → 分发到 `AgentServer` 方法；订阅事件转发。
    ///
    /// 写半归单一 writer task；每个 `Subscribe` 起一个事件转发 task（per-lane
    /// `CancellationToken` 管理，`Unsubscribe` / 连接关闭时 cancel）。连接关闭只
    /// 移除该连接的订阅，不关 session/lane。
    pub async fn serve_connection<S>(self: Arc<Self>, stream: S) -> Result<(), ServerError>
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = LineReader::new(read_half);

        // 写半归单一 writer task：mpsc 汇入 → 写循环。
        let (tx, mut rx) = mpsc::unbounded_channel::<ServerMessage>();
        let (writer_failed_tx, mut writer_failed_rx) = watch::channel(false);
        let writer_task = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if let Err(e) = write_line(&mut write_half, &msg).await {
                    tracing::warn!("server transport: write failed: {e}");
                    let _ = writer_failed_tx.send(true);
                    break;
                }
            }
        });

        // per-lane 订阅（重复订阅 / Unsubscribe / 连接关闭时 cancel 并 await 退出）。
        let mut subscriptions: HashMap<(SessionId, String), Subscription> = HashMap::new();

        // 读循环：逐条解析 ServerRequest 分发；writer 失败时立即终止。
        let result: Result<(), ServerError> = loop {
            let req_result = tokio::select! {
                req = reader.next::<ServerRequest>() => req,
                _ = writer_failed_rx.changed() => {
                    break Err(ServerError::Protocol("writer failed".into()));
                }
            };
            match req_result {
                Ok(Some(req)) => match dispatch(&self, req, &tx, &mut subscriptions).await {
                    Ok(true) => {}
                    Ok(false) => break Ok(()), // Shutdown：关闭连接
                    Err(e) => break Err(e),
                },
                Ok(None) => break Ok(()), // EOF
                Err(e) => break Err(ServerError::Protocol(e.to_string())),
            }
        };

        // 关闭：cancel 所有订阅并等待 forwarder task 退出，再 drop tx 让 writer task 退出。
        for (_, (token, task)) in subscriptions.drain() {
            token.cancel();
            let _ = task.await;
        }
        drop(tx);
        let _ = writer_task.await;

        result
    }
}

/// 取 runtime 工厂并构造 `(config, runtime)`（`SpawnLane` / `ForkLane` 用）。
fn get_runtime(server: &AgentServer) -> Result<(AgentConfig, AgentRuntime), ServerError> {
    let factory = server
        .inner
        .runtime_factory
        .get()
        .cloned()
        .ok_or_else(|| ServerError::Protocol("no runtime factory".into()))?;
    Ok(factory())
}

/// 取 storage 工厂（`CreateSession` / `LoadSession` 用）。
fn get_storage_factory(server: &AgentServer) -> Result<StorageFactory, ServerError> {
    server
        .inner
        .storage_factory
        .get()
        .cloned()
        .ok_or_else(|| ServerError::Protocol("no storage factory".into()))
}

/// 分发一条请求。返回 `false` 表示应关闭连接（Shutdown）。
async fn dispatch(
    server: &AgentServer,
    req: ServerRequest,
    tx: &mpsc::UnboundedSender<ServerMessage>,
    subscriptions: &mut HashMap<(SessionId, String), Subscription>,
) -> Result<bool, ServerError> {
    let id = req.id();
    match req {
        ServerRequest::CreateSession { session_id, .. } => {
            let sid = match session_id {
                Some(sid) => sid,
                None => server
                    .inner
                    .next_session_id
                    .fetch_add(1, Ordering::SeqCst)
                    .to_string(),
            };
            let factory = get_storage_factory(server)?;
            let storage = factory(&sid);
            let result = server
                .create_session(sid.clone(), storage)
                .await
                .map(|_| serde_json::Value::String(sid))
                .map_err(|e| e.to_string());
            send_msg(tx, ServerMessage::Response { id, result })?;
            Ok(true)
        }
        ServerRequest::LoadSession { session_id, .. } => {
            let factory = get_storage_factory(server)?;
            let storage = factory(&session_id);
            let result = server
                .load_session(session_id, storage)
                .await
                .map(|_| serde_json::Value::Null)
                .map_err(|e| e.to_string());
            send_msg(tx, ServerMessage::Response { id, result })?;
            Ok(true)
        }
        ServerRequest::ListSessions { .. } => {
            let sessions = server.list_sessions().await;
            send_msg(tx, ServerMessage::SessionList { id, sessions })?;
            Ok(true)
        }
        ServerRequest::SpawnLane {
            session_id,
            lane_id,
            ..
        } => {
            let (config, runtime) = get_runtime(server)?;
            let result = server
                .spawn_lane(&session_id, &lane_id, config, runtime)
                .await
                .map(|_| serde_json::Value::Null)
                .map_err(|e| e.to_string());
            send_msg(tx, ServerMessage::Response { id, result })?;
            Ok(true)
        }
        ServerRequest::ForkLane {
            session_id,
            from_lane,
            new_lane,
            ..
        } => {
            let (config, runtime) = get_runtime(server)?;
            let result = server
                .fork_lane(&session_id, &from_lane, &new_lane, config, runtime)
                .await
                .map(|_| serde_json::Value::Null)
                .map_err(|e| e.to_string());
            send_msg(tx, ServerMessage::Response { id, result })?;
            Ok(true)
        }
        ServerRequest::Prompt {
            session_id,
            lane_id,
            messages,
            ..
        } => {
            let result = server
                .prompt(&session_id, &lane_id, messages)
                .await
                .map(|_| serde_json::Value::Null)
                .map_err(|e| e.to_string());
            send_msg(tx, ServerMessage::Response { id, result })?;
            Ok(true)
        }
        ServerRequest::Continue {
            session_id,
            lane_id,
            ..
        } => {
            let result = server
                .continue_(&session_id, &lane_id)
                .await
                .map(|_| serde_json::Value::Null)
                .map_err(|e| e.to_string());
            send_msg(tx, ServerMessage::Response { id, result })?;
            Ok(true)
        }
        ServerRequest::Abort {
            session_id,
            lane_id,
            ..
        } => {
            let result = server
                .abort(&session_id, &lane_id)
                .await
                .map(|_| serde_json::Value::Null)
                .map_err(|e| e.to_string());
            send_msg(tx, ServerMessage::Response { id, result })?;
            Ok(true)
        }
        ServerRequest::Reset {
            session_id,
            lane_id,
            ..
        } => {
            let result = server
                .reset(&session_id, &lane_id)
                .await
                .map(|_| serde_json::Value::Null)
                .map_err(|e| e.to_string());
            send_msg(tx, ServerMessage::Response { id, result })?;
            Ok(true)
        }
        ServerRequest::GetSnapshot {
            session_id,
            lane_id,
            ..
        } => {
            match server.snapshot(&session_id, &lane_id).await {
                Some(snapshot) => {
                    send_msg(
                        tx,
                        ServerMessage::Snapshot {
                            session_id,
                            lane_id,
                            snapshot,
                        },
                    )?;
                }
                None => send_msg(
                    tx,
                    ServerMessage::Response {
                        id,
                        result: Err("lane not found".into()),
                    },
                )?,
            }
            Ok(true)
        }
        ServerRequest::Subscribe {
            session_id,
            lane_id,
            ..
        } => {
            match server.subscribe(&session_id, &lane_id).await {
                Some(receiver) => {
                    // 重复订阅同一 (session, lane)：先 cancel 旧 forwarder 并等待其退出，避免重复投递与 task 泄漏。
                    if let Some((old_token, old_task)) =
                        subscriptions.remove(&(session_id.clone(), lane_id.clone()))
                    {
                        old_token.cancel();
                        let _ = old_task.await;
                    }
                    let lane_token = CancellationToken::new();
                    let task = spawn_event_forwarder(
                        tx,
                        &lane_token,
                        session_id.clone(),
                        lane_id.clone(),
                        receiver,
                    );
                    subscriptions.insert((session_id, lane_id), (lane_token, task));
                    send_msg(
                        tx,
                        ServerMessage::Response {
                            id,
                            result: Ok(serde_json::Value::Null),
                        },
                    )?;
                }
                None => send_msg(
                    tx,
                    ServerMessage::Response {
                        id,
                        result: Err("lane not found".into()),
                    },
                )?,
            }
            Ok(true)
        }
        ServerRequest::Unsubscribe {
            session_id,
            lane_id,
            ..
        } => {
            if let Some((token, task)) =
                subscriptions.remove(&(session_id.clone(), lane_id.clone()))
            {
                token.cancel();
                let _ = task.await;
            }
            send_msg(
                tx,
                ServerMessage::Response {
                    id,
                    result: Ok(serde_json::Value::Null),
                },
            )?;
            Ok(true)
        }
        ServerRequest::Shutdown { .. } => {
            // 全局关闭：关闭所有 session/lane（协议语义），再关闭当前连接。
            let result = server
                .shutdown()
                .await
                .map(|_| serde_json::Value::Null)
                .map_err(|e| e.to_string());
            send_msg(tx, ServerMessage::Response { id, result })?;
            Ok(false)
        }
    }
}

/// 启动事件转发 task：消费 lane 的 `broadcast::Receiver`，把 `AgentEvent` 包成 `Event`
/// 消息（带 session/lane 前缀）发到写通道。`token` cancel / 写通道关闭 / 事件流关闭时
/// 退出。返回 `JoinHandle` 供 cancel 后等待 task 真正退出（避免残留 forwarder）。
fn spawn_event_forwarder(
    tx: &mpsc::UnboundedSender<ServerMessage>,
    token: &CancellationToken,
    session_id: SessionId,
    lane_id: String,
    receiver: broadcast::Receiver<AgentEvent>,
) -> tokio::task::JoinHandle<()> {
    let tx = tx.clone();
    let token = token.clone();
    tokio::spawn(async move {
        let mut rx = receiver;
        loop {
            tokio::select! {
                _ = token.cancelled() => break,
                event = rx.recv() => match event {
                    Ok(event) => {
                        if tx
                            .send(ServerMessage::Event {
                                session_id: session_id.clone(),
                                lane_id: lane_id.clone(),
                                event,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                },
            }
        }
    })
}

/// 发送一条 `ServerMessage` 到写通道。
fn send_msg(
    tx: &mpsc::UnboundedSender<ServerMessage>,
    msg: ServerMessage,
) -> Result<(), ServerError> {
    tx.send(msg)
        .map_err(|_| ServerError::Protocol("write channel closed".into()))
}

#[cfg(test)]
mod tests;
