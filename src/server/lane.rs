//! lane 创建与持久化（Task 013，017-b 拆分，018 再拆）：`AgentServer` 的 lane
//! 创建方法与持久化桥接。
//!
//! 从 `mod.rs` 拆出（单文件 ≤ 400 行约束）：`spawn_lane` / `spawn_lane_resumed` /
//! `fork_lane` + 持久化桥接 task `spawn_bridge` + 清理 helper `cleanup_handle`。
//! lane 路由与生命周期（`prompt` / `continue_` / `abort` / `reset` / `snapshot` /
//! `subscribe` / `shutdown`）在 `lane_ops.rs`（017-b 二次拆分）。恢复事务逻辑
//! （`resume_lane_from_factory` / `load_and_resume_session_from_factory`）在
//! `lane_recovery.rs`（018 拆分）。session 注册表与类型定义留在 `mod.rs`。

use std::sync::Arc;

use tokio::sync::{Mutex, broadcast};

use crate::core::agent::{AgentConfig, AgentHandle};
use crate::core::event::AgentEvent;
use crate::core::message::Message;
use crate::core::runtime::AgentRuntime;
use crate::core::session::{LaneWriter, NodeId};

use super::{AgentServer, LaneRuntime, ServerError};

impl AgentServer {
    /// 在 session 内 spawn 一个 lane：spawn `AgentRuntime` → 得到 `AgentHandle`
    /// → 挂 `LaneWriter` 桥接持久化 → 登记。
    ///
    /// 空 transcript 起步，`LaneWriter` head = `None`（首次 append 成为根）。
    /// `session_id` 不存在 → `SessionNotFound`；`lane_id` 已存在 →
    /// `LaneAlreadyExists`。
    pub async fn spawn_lane(
        &self,
        session_id: &str,
        lane_id: &str,
        config: AgentConfig,
        runtime: AgentRuntime,
    ) -> Result<(), ServerError> {
        // 新建 lane：初始 head（None）尚未落盘，须条件式持久化（persist_initial = true）。
        self.spawn_lane_with(session_id, lane_id, config, runtime, Vec::new(), None, true)
            .await
    }

    /// spawn 一个续写 lane（session 恢复用）：runtime 以 `transcript` 为初始
    /// transcript（agent 可见历史上下文），`LaneWriter` head = `head`
    /// （`None` = 空树，首次 append 成为根；`Some(leaf)` = 续写在活动叶之后）。
    ///
    /// `session_id` 不存在 → `SessionNotFound`；`lane_id` 已存在 →
    /// `LaneAlreadyExists`。
    pub async fn spawn_lane_resumed(
        &self,
        session_id: &str,
        lane_id: &str,
        config: AgentConfig,
        runtime: AgentRuntime,
        transcript: Vec<Arc<Message>>,
        head: Option<NodeId>,
    ) -> Result<(), ServerError> {
        // 续写 lane：head 已在日志中（恢复时解析），不重复持久化初始 head
        // （persist_initial = false），避免重启后 `head_committed` 为空导致旧 head
        // 覆盖 bridge 新写入（024 r3 问题 1 的恢复路径变体）。
        self.spawn_lane_with(
            session_id, lane_id, config, runtime, transcript, head, false,
        )
        .await
    }

    /// spawn lane 的共享实现：runtime 以 `transcript` 为初始 transcript，
    /// `LaneWriter` head = `head`。`spawn_lane`（空 transcript / head `None`）与
    /// `spawn_lane_resumed`（恢复 transcript / 活动叶 head）共用。
    ///
    /// 并发：runtime spawn 在锁外，预检查与最终入表不是原子操作。故第二次取锁
    /// 后必须再次校验 session 存在且 lane 不存在；校验失败时显式 shutdown 已
    /// spawn 的 handle（桥接 task 随事件流关闭退出），避免覆盖已有 lane 或泄漏
    /// runtime。
    ///
    /// `persist_initial`（024 r3）区分新建/fork lane（须条件式持久化初始 head）与
    /// 续写 lane（head 已在日志中，跳过，避免重启后覆盖 bridge 新写入）——不可由
    /// 其它参数推导，故保留为独立参数（8 参，超 clippy 默认 7 参上限）。
    #[allow(clippy::too_many_arguments)]
    async fn spawn_lane_with(
        &self,
        session_id: &str,
        lane_id: &str,
        config: AgentConfig,
        runtime: AgentRuntime,
        transcript: Vec<Arc<Message>>,
        head: Option<NodeId>,
        persist_initial: bool,
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
        // 2. spawn runtime（seeded transcript，同步，不入锁）。
        let handle = AgentHandle::spawn_with_transcript(config, runtime, transcript);
        // 3. 建 writer（head = 活动叶 / None）。lane head 持久化由 `storage`
        //    （`SharedSessionStorage`）的 `head_store` 统一决定（024）：绑定时
        //    `append`/`persist_head` 落盘 head，未绑定时 no-op（行为等价 012）。
        let writer = Arc::new(Mutex::new(LaneWriter::new(
            storage.clone(),
            lane_id.to_string(),
            head,
        )));
        // 4. spawn 桥接 task（先于登记订阅，保证事件不丢持久化）。
        let bridge = spawn_bridge(handle.clone(), writer.clone(), lane_id);
        // 5. 二次校验并入表（原子）：session 被并发移除（如 shutdown）或 lane 被
        //    并发登记时，显式清理已 spawn 的 handle 并返回对应错误。登记时分配唯一
        //    generation（024 r3 问题 3），供回滚时校验身份。
        let (insert_err, generation) = {
            let mut sessions = self.inner.sessions.lock().await;
            match sessions.get_mut(session_id) {
                Some(session) => {
                    if session.lanes.contains_key(lane_id) {
                        (Some(ServerError::LaneAlreadyExists(lane_id.to_string())), 0)
                    } else {
                        let generation = self
                            .inner
                            .next_lane_generation
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        session.lanes.insert(
                            lane_id.to_string(),
                            LaneRuntime {
                                lane_id: lane_id.to_string(),
                                handle: handle.clone(),
                                writer: writer.clone(),
                                bridge,
                                generation,
                            },
                        );
                        (None, generation)
                    }
                }
                None => (
                    Some(ServerError::SessionNotFound(session_id.to_string())),
                    0,
                ),
            }
        };
        if let Some(e) = insert_err {
            Self::cleanup_handle(handle).await;
            return Err(e);
        }
        // 6. 条件式落盘初始 head（024 r3 问题 1/2）：**登记成功后**才持久化，且仅当
        //    该 lane 尚无 head 记录时写入（compare-and-append，全程持写锁）——bridge
        //    若已先写入 `H1`，初始 `H0` 被跳过，重放最终 head = 最后一次 append。
        //    `persist_initial = false`（续写 lane，head 已在日志中）跳过本步。
        //    失败则按 generation 回滚登记（仅删本次插入的 runtime）+ 清理 handle。
        if persist_initial && let Err(e) = storage.persist_initial_head(lane_id, head).await {
            self.remove_lane_if_generation(session_id, lane_id, generation)
                .await;
            Self::cleanup_handle(handle).await;
            return Err(ServerError::Session(e));
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
        // 4. 建新 writer，head = 源 head（分支点）。lane head 持久化由 `storage`
        //    （`SharedSessionStorage`）的 `head_store` 统一决定（024）。
        let writer = Arc::new(Mutex::new(LaneWriter::new(
            storage.clone(),
            new_lane.to_string(),
            source_head,
        )));
        // 5. spawn 桥接 task（先于登记订阅，保证事件不丢持久化）。
        let bridge = spawn_bridge(handle.clone(), writer.clone(), new_lane);
        // 6. 二次校验并入表（原子）：session 被并发移除、`new_lane` 被并发登记、
        //    或 `from_lane` 被并发移除时，显式清理已 spawn 的 handle 并返回错误。
        //    登记时分配唯一 generation（024 r3 问题 3），供回滚时校验身份。
        let (insert_err, generation) = {
            let mut sessions = self.inner.sessions.lock().await;
            match sessions.get_mut(session_id) {
                Some(session) => {
                    if session.lanes.contains_key(new_lane) {
                        (
                            Some(ServerError::LaneAlreadyExists(new_lane.to_string())),
                            0,
                        )
                    } else if !session.lanes.contains_key(from_lane) {
                        (Some(ServerError::LaneNotFound(from_lane.to_string())), 0)
                    } else {
                        let generation = self
                            .inner
                            .next_lane_generation
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        session.lanes.insert(
                            new_lane.to_string(),
                            LaneRuntime {
                                lane_id: new_lane.to_string(),
                                handle: handle.clone(),
                                writer: writer.clone(),
                                bridge,
                                generation,
                            },
                        );
                        (None, generation)
                    }
                }
                None => (
                    Some(ServerError::SessionNotFound(session_id.to_string())),
                    0,
                ),
            }
        };
        if let Some(e) = insert_err {
            Self::cleanup_handle(handle).await;
            return Err(e);
        }
        // 7. 条件式落盘初始 head（024 r3 问题 1/2）：**登记成功后**才持久化，且仅当
        //    该 lane 尚无 head 记录时写入（compare-and-append，全程持写锁）——bridge
        //    若已先写入 `H1`，初始分叉点 `H0` 被跳过，新 lane 不回退到分叉点。
        //    失败则按 generation 回滚登记（仅删本次插入的 runtime）+ 清理 handle。
        if let Err(e) = storage.persist_initial_head(new_lane, source_head).await {
            self.remove_lane_if_generation(session_id, new_lane, generation)
                .await;
            Self::cleanup_handle(handle).await;
            return Err(ServerError::Session(e));
        }
        Ok(())
    }

    /// 清理已 spawn 但未登记的 handle：显式 shutdown（等 runtime task 退出），
    /// 桥接 task 随事件流关闭退出。shutdown 失败仅告警（调用方已拿到主错误）。
    async fn cleanup_handle(handle: AgentHandle) {
        if let Err(e) = handle.shutdown().await {
            tracing::warn!("server: failed to clean up unregistered lane handle: {e}");
        }
    }

    /// 按 generation 移除 lane（回滚用，024 r3 问题 3）：仅当 session 与 lane 均
    /// 存在**且 lane 的 generation 匹配**时移除。
    ///
    /// 用于初始 head 持久化失败后的回滚：仅删除本次插入的 `LaneRuntime`（drop 后
    /// 释放 handle/writer/bridge 引用），配合 `cleanup_handle` 显式 shutdown，保证
    /// 「登记 + 落盘 head」要么都成功、要么都不残留。generation 校验杜绝误删并发
    /// 期间（如 shutdown/清理后）以同名 lane 重新登记的 runtime。
    pub(super) async fn remove_lane_if_generation(
        &self,
        session_id: &str,
        lane_id: &str,
        generation: u64,
    ) {
        let mut sessions = self.inner.sessions.lock().await;
        if let Some(session) = sessions.get_mut(session_id) {
            let matches = session
                .lanes
                .get(lane_id)
                .is_some_and(|lane| lane.generation == generation);
            if matches {
                session.lanes.remove(lane_id);
            }
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
