//! lane 写游标（Task 012，024 扩展 head 持久化）：`LaneWriter` 每 lane 独立 `head`
//! 游标，`append` 挂到 `head` 之后并推进；`fork_at` 从任意历史节点分支。
//!
//! 从 `session.rs` 拆出（单文件 ≤ 400 行约束）。`storage` 约束为
//! `Arc<SharedSessionStorage>`（017-a）：类型系统强制 lane 只经共享写入口落盘，
//! 杜绝以裸 `Arc<dyn SessionStorage>` 绕过 `write_lock` 串行化。

use std::sync::Arc;

use super::lane_head::LaneHeadStore;
use super::{LaneId, NodeId, SessionError, SharedSessionStorage};
use crate::core::message::Message;

/// 一个 lane 的写游标：`head` 指向树中本 lane 当前节点，`append` 挂到 `head`
/// 之后并推进；`fork_at` 从任意历史节点分支。
///
/// 单 lane 内 `&mut self` 顺序写；多 lane 并发由各自 `LaneWriter` + 共享
/// `SharedSessionStorage` 的 `write_lock` 保证 `append` 互斥。
///
/// `storage` 约束为 `Arc<SharedSessionStorage>`（017-a）：类型系统强制 lane 只经
/// 共享写入口落盘，杜绝以裸 `Arc<dyn SessionStorage>` 绕过 `write_lock` 串行化。
///
/// lane head 持久化（024）由 `storage`（`SharedSessionStorage`）统一决定：其
/// `head_store` 绑定时 `append` 经 `append_with_head` 原子落盘 head、`persist_head`
/// 落盘当前 head；未绑定时二者为 no-op（行为等价 012）。`LaneWriter` 不另持
/// `head_store` 字段——避免与 `storage` 的 `head_store` 解耦导致「内存 head 已推进
/// 但磁盘未落盘」的分裂（024 r1 问题 1/2）。
pub struct LaneWriter {
    storage: Arc<SharedSessionStorage>,
    lane_id: LaneId,
    head: Option<NodeId>,
}

impl LaneWriter {
    /// 创建 lane 写游标。`head = None` 表示尚无节点（首次 `append` 成为根）。
    ///
    /// lane head 持久化由 `storage`（`SharedSessionStorage`）的 `head_store` 决定：
    /// 绑定时 `append` 自动原子落盘 head、`persist_head` 落盘当前 head；未绑定时
    /// 行为与 012 完全一致（纯 message append，head 不落盘）。
    pub fn new(
        storage: Arc<SharedSessionStorage>,
        lane_id: impl Into<String>,
        head: Option<NodeId>,
    ) -> Self {
        Self {
            storage,
            lane_id: lane_id.into(),
            head,
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
    /// 经 `storage` 的 `LaneHeadStore` 落盘：`storage` 未绑定 head 持久化时为空
    /// 操作（行为等价 012）。`&self`：写盘在 `storage` 内部串行，不持跨 await 锁。
    pub async fn persist_head(&self) -> Result<(), SessionError> {
        self.storage
            .append_lane_head(self.lane_id.clone(), self.head)
            .await
    }

    /// 追加一条消息为 `head` 的子节点，推进 `head`，返回新节点 id。
    ///
    /// 经 `SharedSessionStorage::append_with_head` 在同一写锁内原子提交 message 与
    /// 新 head（024）：`storage` 绑定 head 持久化时 head 落盘失败则整体返回错误、
    /// **不推进内存 head**，杜绝内存与磁盘 head 分裂；未绑定时等价纯 message append
    /// （012 行为）。仅提交成功后才推进内存 head。
    pub async fn append(&mut self, message: Message) -> Result<NodeId, SessionError> {
        let id = self
            .storage
            .append_with_head(&self.lane_id, self.head, message)
            .await?;
        // 仅提交成功后才推进内存 head；失败时 head 保持旧值（调用方据此停写）。
        self.head = Some(id);
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
