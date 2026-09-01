//! RemoteServer：把 `AgentHandle` 的命令面 + 事件流暴露到字节流。
//!
//! 并发模型：写半归单一 writer task（`mpsc` 汇入 → 写循环）；事件订阅转发
//! 归单独 task（`broadcast` recv → `mpsc`）。读循环在 `serve` 主 task 内，
//! 不持锁跨 await。`serve` 取 `&self`（`AgentHandle` 为 `Clone`，`Shutdown`
//! 时 clone 后消费以干净关闭 runtime）。

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, mpsc};

use super::codec::{LineReader, write_line};
use super::protocol::{RemoteError, RemoteRequest, ServerMessage};
use crate::core::agent::{Agent, AgentHandle};

/// 远程服务端：把 `AgentHandle` 的命令面 + 事件流暴露到字节流。
pub struct RemoteServer {
    handle: AgentHandle,
}

impl RemoteServer {
    /// 创建服务端。
    pub fn new(handle: AgentHandle) -> Self {
        Self { handle }
    }

    /// 服务一条连接：先推初始 Snapshot，再读 RemoteRequest 分发，事件订阅转发。
    ///
    /// 取 `&self`：`AgentHandle` 为 `Clone`，`Shutdown` 时 clone 后消费以干净
    /// 关闭 runtime，原 handle 保留在 server 内。
    pub async fn serve<S>(&self, stream: S) -> Result<(), RemoteError>
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = LineReader::new(read_half);

        // 写半归单一 writer task：mpsc 汇入 → 写循环。
        let (tx, mut rx) = mpsc::unbounded_channel::<ServerMessage>();

        // 事件订阅转发 task。
        let tx_event = tx.clone();
        let mut events_rx = self.handle.subscribe();
        let event_task = tokio::spawn(async move {
            loop {
                match events_rx.recv().await {
                    Ok(event) => {
                        if send_msg(&tx_event, ServerMessage::Event { event }).is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        // writer task：从 mpsc 取消息写入写半。
        let writer_task = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if let Err(e) = write_line(&mut write_half, &msg).await {
                    tracing::warn!("remote server: write failed: {e}");
                    break;
                }
            }
        });

        // 先发初始 Snapshot（id = 0 保留）。
        let initial = self.handle.snapshot();
        if send_msg(
            &tx,
            ServerMessage::Snapshot {
                id: 0,
                snapshot: initial,
            },
        )
        .is_err()
        {
            event_task.abort();
            let _ = writer_task.await;
            return Ok(());
        }

        // 读循环：逐条解析 RemoteRequest 分发。
        let result = loop {
            match reader.next::<RemoteRequest>().await? {
                Some(req) => {
                    let keep_going = dispatch(&self.handle, req, &tx).await?;
                    if !keep_going {
                        // Shutdown：clone handle 后消费，干净关闭 runtime。
                        self.handle
                            .clone()
                            .shutdown()
                            .await
                            .map_err(|e| RemoteError::Command(e.to_string()))?;
                        break Ok(());
                    }
                }
                None => break Ok(()), // EOF
            }
        };

        // 关闭：drop tx 让 writer task 退出，等待 writer task 完成。
        drop(tx);
        event_task.abort();
        let _ = writer_task.await;

        result
    }
}

/// 分发一条请求。返回 `false` 表示应关闭连接（Shutdown）。
async fn dispatch(
    handle: &AgentHandle,
    req: RemoteRequest,
    tx: &mpsc::UnboundedSender<ServerMessage>,
) -> Result<bool, RemoteError> {
    let id = req.id();
    match req {
        RemoteRequest::Prompt { messages, .. } => {
            let result = handle.prompt(messages).await.map_err(|e| e.to_string());
            send_msg(tx, ServerMessage::Response { id, result })?;
            Ok(true)
        }
        RemoteRequest::Continue { .. } => {
            let result = handle.continue_().await.map_err(|e| e.to_string());
            send_msg(tx, ServerMessage::Response { id, result })?;
            Ok(true)
        }
        RemoteRequest::Steer { message, .. } => {
            let result = handle.steer(message).await.map_err(|e| e.to_string());
            send_msg(tx, ServerMessage::Response { id, result })?;
            Ok(true)
        }
        RemoteRequest::FollowUp { message, .. } => {
            let result = handle.follow_up(message).await.map_err(|e| e.to_string());
            send_msg(tx, ServerMessage::Response { id, result })?;
            Ok(true)
        }
        RemoteRequest::Abort { .. } => {
            handle.abort();
            send_msg(tx, ServerMessage::Response { id, result: Ok(()) })?;
            Ok(true)
        }
        RemoteRequest::Reset { .. } => {
            let result = handle.reset().await.map_err(|e| e.to_string());
            send_msg(tx, ServerMessage::Response { id, result })?;
            Ok(true)
        }
        RemoteRequest::GetSnapshot { .. } => {
            let snapshot = handle.snapshot();
            send_msg(tx, ServerMessage::Snapshot { id, snapshot })?;
            Ok(true)
        }
        RemoteRequest::Shutdown { .. } => {
            send_msg(tx, ServerMessage::Response { id, result: Ok(()) })?;
            Ok(false)
        }
    }
}

/// 发送一条 `ServerMessage` 到写通道。
fn send_msg(
    tx: &mpsc::UnboundedSender<ServerMessage>,
    msg: ServerMessage,
) -> Result<(), RemoteError> {
    tx.send(msg)
        .map_err(|_| RemoteError::Protocol("write channel closed".into()))
}

#[cfg(test)]
mod tests;
