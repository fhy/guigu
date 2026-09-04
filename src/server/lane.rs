//! lane 调度（Task 013）：`AgentServer` 的 lane 生命周期与路由方法。
//!
//! 从 `mod.rs` 拆出（单文件 ≤ 400 行约束）：`spawn_lane` / `fork_lane` /
//! `prompt` / `continue_` / `abort` / `reset` / `snapshot` / `subscribe` /
//! `shutdown` + 持久化桥接 task `spawn_bridge`。session 注册表与类型定义留在
//! `mod.rs`。

use std::sync::Arc;

use tokio::sync::{Mutex, broadcast};

use crate::core::agent::{Agent, AgentConfig, AgentHandle, AgentSnapshot};
use crate::core::event::AgentEvent;
use crate::core::message::Message;
use crate::core::runtime::AgentRuntime;
use crate::core::session::LaneWriter;

use super::{AgentServer, LaneRuntime, ServerError};

impl AgentServer {
    /// 在 session 内 spawn 一个 lane：spawn `AgentRuntime` → 得到 `AgentHandle`
    /// → 挂 `LaneWriter` 桥接持久化 → 登记。
    ///
    /// `session_id` 不存在 → `SessionNotFound`；`lane_id` 已存在 →
    /// `LaneAlreadyExists`。
    ///
    /// 并发：runtime spawn 在锁外，预检查与最终入表不是原子操作。故第二次取锁
    /// 后必须再次校验 session 存在且 lane 不存在；校验失败时显式 shutdown 已
    /// spawn 的 handle（桥接 task 随事件流关闭退出），避免覆盖已有 lane 或泄漏
    /// runtime。
    pub async fn spawn_lane(
        &self,
        session_id: &str,
        lane_id: &str,
        config: AgentConfig,
        runtime: AgentRuntime,
    ) -> Result<(), ServerError> {
        // 1. 检查 session 存在 + lane 不存在（不跨 await 持锁）。
        let storage = {
            let sessions = self.inner.sessions.lock().await;
            let session = sessions
                .get(session_id)
                .ok_or_else(|| ServerError::SessionNotFound(session_id.to_string()))?;
            if session.lanes.contains_key(lane_id) {
                return Err(ServerError::LaneAlreadyExists(lane_id.to_string()));
            }
            session.storage.clone()
        };
        // 2. spawn runtime（同步，不入锁）。
        let handle = AgentHandle::spawn(config, runtime);
        // 3. 建 writer（head = None，首次 append 成为根）。
        let writer = Arc::new(Mutex::new(LaneWriter::new(
            storage,
            lane_id.to_string(),
            None,
        )));
        // 4. spawn 桥接 task（先于登记订阅，保证事件不丢持久化）。
        let bridge = spawn_bridge(handle.clone(), writer.clone(), lane_id);
        // 5. 二次校验并入表（原子）：session 被并发移除（如 shutdown）或 lane 被
        //    并发登记时，显式清理已 spawn 的 handle 并返回对应错误。
        let insert_err = {
            let mut sessions = self.inner.sessions.lock().await;
            match sessions.get_mut(session_id) {
                Some(session) => {
                    if session.lanes.contains_key(lane_id) {
                        Some(ServerError::LaneAlreadyExists(lane_id.to_string()))
                    } else {
                        session.lanes.insert(
                            lane_id.to_string(),
                            LaneRuntime {
                                lane_id: lane_id.to_string(),
                                handle: handle.clone(),
                                writer: writer.clone(),
                                bridge,
                            },
                        );
                        None
                    }
                }
                None => Some(ServerError::SessionNotFound(session_id.to_string())),
            }
        };
        if let Some(e) = insert_err {
            Self::cleanup_handle(handle).await;
            return Err(e);
        }
        Ok(())
    }

    /// 从 `from_lane` 的当前 head 分支出新 lane（新 runtime），后续写落到新分支。
    ///
    /// 新 lane 的 `LaneWriter` `fork_at` 到源 lane 的 head（012）；新 runtime 从空
    /// transcript 起步（`AgentHandle` 不支持种子 transcript，不改既有签名）。
    /// `session_id` 不存在 → `SessionNotFound`；`from_lane` 不存在 → `LaneNotFound`；
    /// `new_lane` 已存在 → `LaneAlreadyExists`。
    ///
    /// 并发：与 `spawn_lane` 相同——runtime spawn 在锁外，第二次取锁后再次校验
    /// session 存在、`new_lane` 不存在、`from_lane` 仍存在；校验失败时显式
    /// shutdown 已 spawn 的 handle，避免覆盖已有 lane 或泄漏 runtime。
    pub async fn fork_lane(
        &self,
        session_id: &str,
        from_lane: &str,
        new_lane: &str,
        config: AgentConfig,
        runtime: AgentRuntime,
    ) -> Result<(), ServerError> {
        // 1. 读源 lane 的 writer + storage，检查新 lane 不存在（不跨 await 持锁）。
        let (storage, source_writer) = {
            let sessions = self.inner.sessions.lock().await;
            let session = sessions
                .get(session_id)
                .ok_or_else(|| ServerError::SessionNotFound(session_id.to_string()))?;
            if session.lanes.contains_key(new_lane) {
                return Err(ServerError::LaneAlreadyExists(new_lane.to_string()));
            }
            let lane = session
                .lanes
                .get(from_lane)
                .ok_or_else(|| ServerError::LaneNotFound(from_lane.to_string()))?;
            (session.storage.clone(), lane.writer.clone())
        };
        // 2. 读源 lane 的 head（锁 writer，同步读）。
        let source_head = source_writer.lock().await.head();
        // 3. spawn 新 runtime（同步，不入锁）。
        let handle = AgentHandle::spawn(config, runtime);
        // 4. 建新 writer，fork_at 源 head（分支点）。
        let writer = Arc::new(Mutex::new(LaneWriter::new(
            storage,
            new_lane.to_string(),
            source_head,
        )));
        // 5. spawn 桥接 task（先于登记订阅，保证事件不丢持久化）。
        let bridge = spawn_bridge(handle.clone(), writer.clone(), new_lane);
        // 6. 二次校验并入表（原子）：session 被并发移除、`new_lane` 被并发登记、
        //    或 `from_lane` 被并发移除时，显式清理已 spawn 的 handle 并返回错误。
        let insert_err = {
            let mut sessions = self.inner.sessions.lock().await;
            match sessions.get_mut(session_id) {
                Some(session) => {
                    if session.lanes.contains_key(new_lane) {
                        Some(ServerError::LaneAlreadyExists(new_lane.to_string()))
                    } else if !session.lanes.contains_key(from_lane) {
                        Some(ServerError::LaneNotFound(from_lane.to_string()))
                    } else {
                        session.lanes.insert(
                            new_lane.to_string(),
                            LaneRuntime {
                                lane_id: new_lane.to_string(),
                                handle: handle.clone(),
                                writer: writer.clone(),
                                bridge,
                            },
                        );
                        None
                    }
                }
                None => Some(ServerError::SessionNotFound(session_id.to_string())),
            }
        };
        if let Some(e) = insert_err {
            Self::cleanup_handle(handle).await;
            return Err(e);
        }
        Ok(())
    }

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

    /// 清理已 spawn 但未登记的 handle：显式 shutdown（等 runtime task 退出），
    /// 桥接 task 随事件流关闭退出。shutdown 失败仅告警（调用方已拿到主错误）。
    async fn cleanup_handle(handle: AgentHandle) {
        if let Err(e) = handle.shutdown().await {
            tracing::warn!("server: failed to clean up unregistered lane handle: {e}");
        }
    }
}

/// 启动 lane 持久化桥接 task：订阅 lane 事件流，对 `MessageEnd` 经 `LaneWriter`
/// 串行落盘（复用 012 `LaneWriter` + 009 `SessionRecorder::attach` 思想）。
///
/// 只把 `broadcast::Receiver` 移入 task（`handle` 在 `subscribe()` 后即 drop），
/// 使 lane shutdown 后 broadcast sender 归零、task 的 `rx` 收到 `Closed` 而退出。
///
/// 返回 `JoinHandle`：`AgentServer::shutdown` 等其完成，保证进程退出前 `MessageEnd`
/// 全部落盘（否则 `#[tokio::main]` drop runtime 时取消桥接 task，末条持久化丢失）。
fn spawn_bridge(
    handle: AgentHandle,
    writer: Arc<Mutex<LaneWriter>>,
    lane_id: &str,
) -> tokio::task::JoinHandle<()> {
    let rx = handle.subscribe();
    let lane_id = lane_id.to_string();
    tokio::spawn(async move {
        let mut rx = rx;
        loop {
            match rx.recv().await {
                Ok(AgentEvent::MessageEnd { message }) => {
                    let mut writer = writer.lock().await;
                    if let Err(e) = writer.append((*message).clone()).await {
                        tracing::warn!("server: lane {lane_id} persist failed: {e}");
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("server: lane {lane_id} persist lagged, skipped {n}");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}
