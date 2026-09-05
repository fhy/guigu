//! lane 路由与生命周期（Task 017-b 拆分）：`AgentServer` 的 lane 消息路由与
//! 关闭方法。
//!
//! 从 `lane.rs` 拆出（单文件 ≤ 400 行约束）：`prompt` / `continue_` / `abort` /
//! `reset` / `snapshot` / `subscribe` / `shutdown` + 私有 `get_lane`。lane 创建
//! 与持久化桥接（`spawn_lane` / `fork_lane` / `resume_lane_from_factory` /
//! `spawn_bridge`）留在 `lane.rs`。

use tokio::sync::broadcast;

use crate::core::agent::{Agent, AgentHandle, AgentSnapshot};
use crate::core::event::AgentEvent;
use crate::core::message::Message;

use super::{AgentServer, ServerError};

impl AgentServer {
    /// 向 lane 发送提示消息（路由到 lane 的 `AgentHandle`）。
    pub async fn prompt(
        &self,
        session_id: &str,
        lane_id: &str,
        messages: Vec<Message>,
    ) -> Result<(), ServerError> {
        let handle = self.get_lane(session_id, lane_id).await?;
        handle
            .prompt(messages)
            .await
            .map_err(|e| ServerError::Agent(e.to_string()))
    }

    /// 继续处理（路由到 lane 的 `AgentHandle`）。
    pub async fn continue_(&self, session_id: &str, lane_id: &str) -> Result<(), ServerError> {
        let handle = self.get_lane(session_id, lane_id).await?;
        handle
            .continue_()
            .await
            .map_err(|e| ServerError::Agent(e.to_string()))
    }

    /// 中止当前操作（路由到 lane 的 `AgentHandle`，非阻塞）。
    pub async fn abort(&self, session_id: &str, lane_id: &str) -> Result<(), ServerError> {
        let handle = self.get_lane(session_id, lane_id).await?;
        handle.abort();
        Ok(())
    }

    /// 重置 lane（路由到 lane 的 `AgentHandle`）。
    pub async fn reset(&self, session_id: &str, lane_id: &str) -> Result<(), ServerError> {
        let handle = self.get_lane(session_id, lane_id).await?;
        handle
            .reset()
            .await
            .map_err(|e| ServerError::Agent(e.to_string()))
    }

    /// 获取 lane 当前快照；session / lane 不存在返回 `None`（不 panic）。
    pub async fn snapshot(&self, session_id: &str, lane_id: &str) -> Option<AgentSnapshot> {
        let sessions = self.inner.sessions.lock().await;
        Some(
            sessions
                .get(session_id)?
                .lanes
                .get(lane_id)?
                .handle
                .snapshot(),
        )
    }

    /// 订阅 lane 事件；session / lane 不存在返回 `None`（不 panic）。
    pub async fn subscribe(
        &self,
        session_id: &str,
        lane_id: &str,
    ) -> Option<broadcast::Receiver<AgentEvent>> {
        let sessions = self.inner.sessions.lock().await;
        Some(
            sessions
                .get(session_id)?
                .lanes
                .get(lane_id)?
                .handle
                .subscribe(),
        )
    }

    /// 关闭 server：shutdown 所有 lane 并清空注册表，**等桥接 task 落盘完成**。
    ///
    /// 顺序：克隆 handle + 桥接 `JoinHandle` → 清空注册表 → 逐个 shutdown runtime
    /// task（消费 handle，broadcast sender 归零）→ 等桥接 task 退出。桥接 task 在
    /// sender 归零后处理完缓冲的 `MessageEnd` 才退出，故此处返回即保证持久化落盘
    /// （否则 `#[tokio::main]` drop runtime 时取消桥接 task，末条持久化丢失）。
    pub async fn shutdown(&self) -> Result<(), ServerError> {
        // 取走全部 session（drain 注册表），移出 handle + 桥接 JoinHandle
        // （`JoinHandle` 不可 Clone，故直接移动而非克隆）。
        let sessions: Vec<_> = {
            let mut guard = self.inner.sessions.lock().await;
            guard.drain().map(|(_, s)| s).collect()
        };
        let mut handles = Vec::new();
        let mut bridges = Vec::new();
        for session in sessions {
            for lane in session.lanes.into_values() {
                handles.push(lane.handle);
                bridges.push(lane.bridge);
            }
        }
        for handle in handles {
            handle
                .shutdown()
                .await
                .map_err(|e| ServerError::Agent(e.to_string()))?;
        }
        // handles 已消费（broadcast sender 归零）→ 桥接 task 处理完缓冲事件后退出。
        // 等其完成，保证 `MessageEnd` 全部落盘。
        for bridge in bridges {
            if let Err(e) = bridge.await {
                tracing::warn!("server: bridge task panicked: {e}");
            }
        }
        Ok(())
    }

    /// 取 lane 的 `AgentHandle`（克隆）；session / lane 不存在返回错误。
    async fn get_lane(&self, session_id: &str, lane_id: &str) -> Result<AgentHandle, ServerError> {
        let sessions = self.inner.sessions.lock().await;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| ServerError::SessionNotFound(session_id.to_string()))?;
        let lane = session
            .lanes
            .get(lane_id)
            .ok_or_else(|| ServerError::LaneNotFound(lane_id.to_string()))?;
        Ok(lane.handle.clone())
    }
}
