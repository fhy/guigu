//! lane 写游标（Task 012，024 扩展 head 持久化）：`LaneWriter` 每 lane 独立 `head`
//! 游标，`append` 挂到 `head` 之后并推进；`fork_at` 从任意历史节点分支。
//!
//! 从 `session.rs` 拆出（单文件 ≤ 400 行约束）。`storage` 约束为
//! `Arc<SharedSessionStorage>`（017-a）：类型系统强制 lane 只经共享写入口落盘，
//! 杜绝以裸 `Arc<dyn SessionStorage>` 绕过 `write_lock` 串行化。

use std::sync::Arc;

use super::lane_head::LaneHeadStore;
use super::{LaneId, NodeId, SessionError, SessionStorage, SharedSessionStorage};
use crate::core::message::Message;

/// 一个 lane 的写游标：`head` 指向树中本 lane 当前节点，`append` 挂到 `head`
/// 之后并推进；`fork_at` 从任意历史节点分支。
///
/// 单 lane 内 `&mut self` 顺序写；多 lane 并发由各自 `LaneWriter` + 共享
/// `SharedSessionStorage` 的 `write_lock` 保证 `append` 互斥。
///
/// `storage` 约束为 `Arc<SharedSessionStorage>`（017-a）：类型系统强制 lane 只经
/// 共享写入口落盘，杜绝以裸 `Arc<dyn SessionStorage>` 绕过 `write_lock` 串行化。
pub struct LaneWriter {
    storage: Arc<SharedSessionStorage>,
    lane_id: LaneId,
    head: Option<NodeId>,
    /// 可选 lane head 持久化后端（024）：`None` 时 `append` 不落盘 head，行为与
    /// 012 完全一致。通常与 `storage` 指向同一后端实例。
    head_store: Option<Arc<dyn LaneHeadStore>>,
}

impl LaneWriter {
    /// 创建 lane 写游标。`head = None` 表示尚无节点（首次 `append` 成为根）。
    ///
    /// 不绑定 head 持久化（`head_store = None`），行为与 012 完全一致。
    pub fn new(
        storage: Arc<SharedSessionStorage>,
        lane_id: impl Into<String>,
        head: Option<NodeId>,
    ) -> Self {
        Self {
            storage,
            lane_id: lane_id.into(),
            head,
            head_store: None,
        }
    }

    /// 创建 lane 写游标并绑定 lane head 持久化（024）：`append` 成功后自动落盘 head。
    pub fn with_head_store(
        storage: Arc<SharedSessionStorage>,
        lane_id: impl Into<String>,
        head: Option<NodeId>,
        head_store: Arc<dyn LaneHeadStore>,
    ) -> Self {
        Self {
            storage,
            lane_id: lane_id.into(),
            head,
            head_store: Some(head_store),
        }
    }

    /// lane 标识。
    pub fn lane_id(&self) -> &LaneId {
        &self.lane_id
    }

    /// 当前 head（`None` = 尚无节点）。
    pub fn head(&self) -> Option<NodeId> {
        self.head
    }

    /// 显式落盘当前 head（lane 创建/fork 后调用一次，幂等）。
    ///
    /// 未绑定 head 持久化时为空操作。`&self`：`head_store` 是 `Arc`，写盘不持跨
    /// await 锁（与 012 `write_lock` 只在 `storage.append` 内串行一致）。
    pub async fn persist_head(&self) -> Result<(), SessionError> {
        if let Some(store) = &self.head_store {
            store
                .append_lane_head(self.lane_id.clone(), self.head)
                .await?;
        }
        Ok(())
    }

    /// 追加一条消息为 `head` 的子节点，推进 `head`，返回新节点 id。
    ///
    /// 绑定 head 持久化时，append 成功后自动落盘新 head（内聚覆盖 append 推进）。
    pub async fn append(&mut self, message: Message) -> Result<NodeId, SessionError> {
        let id = self.storage.append(self.head, message).await?;
        self.head = Some(id);
        if let Some(store) = &self.head_store {
            store
                .append_lane_head(self.lane_id.clone(), self.head)
                .await?;
        }
        Ok(id)
    }

    /// 从指定历史节点 fork：后续 `append` 挂到该节点之后（产生分支）。
    ///
    /// `parent = None` 表示重置到「无 head」，下次 `append` 成为新根——仅对空树
    /// 合法；非空树会产生多根，`load` / `reduce` 以 `MultipleRoots` 拒绝。
    ///
    /// 纯内存操作（不落盘，保持 012 同步签名）；spawn/fork 后由调用方显式
    /// `persist_head` 落初始 head，或后续 `append` 自动落盘。
    pub fn fork_at(&mut self, parent: Option<NodeId>) {
        self.head = parent;
    }
}
