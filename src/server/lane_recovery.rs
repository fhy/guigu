//! lane 恢复事务逻辑（Task 018 拆分）：`AgentServer` 的 session 恢复 / 续写入口
//! 与事务性 load + 注册 + spawn。
//!
//! 从 `lane.rs` 拆出（单文件 ≤ 400 行约束）：`resume_lane_from_factory`（已注册
//! session 恢复）/ `load_and_resume_session_from_factory`（事务式 load + 注册 +
//! spawn，spawn 失败回滚空 session）+ 回滚 helper `remove_session_if_empty` +
//! 共享 head 解析 `resolve_resume_head` / `transcript_from_path`。lane 创建与
//! 持久化桥接（`spawn_lane` / `fork_lane` / `spawn_bridge`）留在 `lane.rs`。
//!
//! 恢复事务语义（017-b 定稿，不改）：`session/load` 事务化（先校验 `head` 再
//! 注册）、spawn 失败回滚空 session、`path_to(h)` 仅限叶节点契约。

use std::collections::HashMap;
use std::sync::Arc;

use crate::core::message::Message;
use crate::core::session::{NodeId, SessionStorage, SessionTree, SharedSessionStorage};

use super::{AgentServer, ServerError, SessionId, SessionState};

impl AgentServer {
    /// 恢复 session 并 spawn 一个续写 lane（崩溃恢复 / `--session` 续聊入口）：
    /// load 树 → 定目标 head → 以根→叶消息序列初始化 runtime transcript →
    /// `LaneWriter` head = 目标 head。
    ///
    /// `head` 语义（017-b）：
    /// - `Some(h)`：显式目标 head（须为叶节点）。transcript = `path_to(h)`，
    ///   `LaneWriter` head = `h`。`h` 不在树中或为内部节点（`path_to` 返回
    ///   `None`）→ **显式返回 `ServerError::Protocol`**，不静默回退到 max NodeId
    ///   叶，避免掩盖调用方传入非法/过期 head 的 bug。从内部节点开新分支属
    ///   `fork_lane` 职责，非本恢复入口。
    /// - `None`：取 max NodeId 叶作为活动叶（兼容单 lane 续聊；空树 → `None`，
    ///   首次 append 成为根）。NodeId 单调递增，最新 append 的节点必为叶（若其
    ///   有子节点则子节点 id 更大，矛盾），故 max 叶即最近续写点。
    ///
    /// `session_id` 须已注册（`load_session` / `create_session`）；runtime 工厂未
    /// 配置 → `Protocol` 错误；`lane_id` 已存在 → `LaneAlreadyExists`。
    pub async fn resume_lane_from_factory(
        &self,
        session_id: &str,
        lane_id: &str,
        head: Option<NodeId>,
    ) -> Result<(), ServerError> {
        let factory = self
            .inner
            .runtime_factory
            .get()
            .cloned()
            .ok_or_else(|| ServerError::Protocol("no runtime factory".into()))?;
        let (config, runtime) = factory();

        // 取 session storage（不跨 await 持锁）。
        let storage = {
            let sessions = self.inner.sessions.lock().await;
            sessions
                .get(session_id)
                .ok_or_else(|| ServerError::SessionNotFound(session_id.to_string()))?
                .storage
                .clone()
        };
        // load 树（锁外）。
        let tree = storage.load().await?;
        // 定目标 head + 叶路径 transcript（共享逻辑，非法 head → Protocol）。
        let (head, transcript) = resolve_resume_head(&tree, head)?;
        self.spawn_lane_resumed(session_id, lane_id, config, runtime, transcript, head)
            .await
    }

    /// 从持久化 load + 校验 head + 注册 session + spawn 续写 lane（**事务式**，
    /// 017-b 修复）。
    ///
    /// 与「先 `load_session_from_factory` 再 `resume_lane_from_factory`」的两步
    /// 流程不同：本方法在**注册 session 之前**先 load 树并校验显式 `head`。head
    /// 非法（不在树中或为内部节点）时直接返回 `Protocol` 错误，**不注册
    /// session**，避免非法请求在注册表残留无 lane 的 session（后续同 id 重试会
    /// `DuplicateSession`，状态污染）。
    ///
    /// `head` 语义同 `resume_lane_from_factory`（见 `resolve_resume_head`）：
    /// - `Some(h)`：显式目标叶节点；`path_to(h)` 失败 → `Protocol`（不静默回退）。
    /// - `None`：取 max NodeId 叶（空树 → `None`）。
    ///
    /// 事务性：注册 session 后若 `spawn_lane_resumed` 失败（如并发 shutdown），
    /// 回滚本次注册的 session（仅当仍为空），保证「session + lane 要么都成功，
    /// 要么都不残留」。
    pub async fn load_and_resume_session_from_factory(
        &self,
        session_id: SessionId,
        lane_id: &str,
        head: Option<NodeId>,
    ) -> Result<(), ServerError> {
        // 1. storage 工厂（未配置 → Protocol）。
        let storage_factory = self
            .inner
            .storage_factory
            .get()
            .cloned()
            .ok_or_else(|| ServerError::Protocol("no storage factory".into()))?;
        let storage = Arc::new(SharedSessionStorage::new(storage_factory(&session_id)));
        // 2. load 树（锁外）。
        let tree = storage.load().await?;
        // 3. 定目标 head + transcript（**注册前**校验显式 head：非法 → Protocol，
        //    不注册 session）。
        let (head, transcript) = resolve_resume_head(&tree, head)?;
        // 4. runtime 工厂（未配置 → Protocol）。
        let runtime_factory = self
            .inner
            .runtime_factory
            .get()
            .cloned()
            .ok_or_else(|| ServerError::Protocol("no runtime factory".into()))?;
        let (config, runtime) = runtime_factory();
        // 5. 注册 session（原子：已存在 → DuplicateSession）。
        {
            let mut sessions = self.inner.sessions.lock().await;
            if sessions.contains_key(&session_id) {
                return Err(ServerError::DuplicateSession(session_id));
            }
            sessions.insert(
                session_id.clone(),
                SessionState {
                    session_id: session_id.clone(),
                    storage: storage.clone(),
                    lanes: HashMap::new(),
                },
            );
        }
        // 6. spawn 续写 lane（session 已注册）。失败时回滚本次注册的 session，
        //    保证事务性。
        if let Err(e) = self
            .spawn_lane_resumed(&session_id, lane_id, config, runtime, transcript, head)
            .await
        {
            self.remove_session_if_empty(&session_id).await;
            return Err(e);
        }
        Ok(())
    }

    /// 移除仍为空的 session（回滚用）：仅当 session 存在且无任何 lane 时移除，
    /// 避免误删并发写入的 session。
    async fn remove_session_if_empty(&self, session_id: &str) {
        let mut sessions = self.inner.sessions.lock().await;
        let is_empty = sessions
            .get(session_id)
            .map(|s| s.lanes.is_empty())
            .unwrap_or(false);
        if is_empty {
            sessions.remove(session_id);
        }
    }
}

/// 把根→叶消息序列转为 runtime transcript（克隆为 `Arc`）。
fn transcript_from_path(path: &[&Message]) -> Vec<Arc<Message>> {
    path.iter().map(|m| Arc::new((**m).clone())).collect()
}

/// 从树定目标 head + 叶路径 transcript（017-b 共享逻辑）。
///
/// `resume_lane_from_factory`（已注册 session）与
/// `load_and_resume_session_from_factory`（事务式 load + 注册）共用，保证两处
/// head 语义一致：
/// - `Some(h)`：显式目标叶节点（009 契约 `path_to(leaf)`）；`path_to(h)` 失败
///   （不在树中或为内部节点）→ **显式 `Protocol` 错误**，不静默回退到 max NodeId
///   叶，避免掩盖调用方传入非法/过期 head 的 bug。
/// - `None`：取 max NodeId 叶作为活动叶（空树 → `None`，空 transcript）。NodeId
///   单调递增，最新 append 的节点必为叶（若其有子节点则子节点 id 更大，矛盾），
///   故 max 叶即最近续写点。
fn resolve_resume_head(
    tree: &SessionTree,
    head: Option<NodeId>,
) -> Result<(Option<NodeId>, Vec<Arc<Message>>), ServerError> {
    match head {
        Some(h) => {
            let path = tree.path_to(h).ok_or_else(|| {
                ServerError::Protocol(format!(
                    "resume head {h} is not a leaf node (unknown or internal)"
                ))
            })?;
            Ok((Some(h), transcript_from_path(&path)))
        }
        None => match tree.leaves().into_iter().max() {
            Some(leaf) => {
                let path = tree
                    .path_to(leaf)
                    .ok_or_else(|| ServerError::Protocol("active leaf path unavailable".into()))?;
                Ok((Some(leaf), transcript_from_path(&path)))
            }
            None => Ok((None, Vec::new())),
        },
    }
}
