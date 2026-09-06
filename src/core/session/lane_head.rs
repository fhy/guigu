//! lane head 元数据持久化（Task 024）：`LaneHeadRecord`（JSONL 行）+ `SessionRecord`
//! （untagged 区分 Message/LaneHead）+ `LaneHeadStore` 可选 trait。
//!
//! 从 `session.rs` 拆出（单文件 ≤ 400 行约束）。字段互斥是 `SessionRecord` untagged
//! 可靠区分的前提：`SessionEntry = {id, parent_id, message}`，`LaneHeadRecord =
//! {lane_id, head}`，二者字段集合不重叠。

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{LaneId, NodeId, SessionEntry, SessionError};

/// lane head 变更记录：某 lane 活动分支头指针（JSONL 一行，append-only）。
///
/// 与 `SessionEntry` 字段互斥（`SessionEntry = {id, parent_id, message}`，本结构
/// `= {lane_id, head}`），是 `SessionRecord` untagged 可靠区分的前提。若未来给
/// `SessionEntry` 增加 `lane_id` 字段将破坏互斥，需同步改用显式 tag。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaneHeadRecord {
    /// lane 标识。
    pub lane_id: LaneId,
    /// 活动分支头；`None` = lane 已建但尚无节点。
    pub head: Option<NodeId>,
}

/// JSONL 每行：消息节点 或 lane head 元数据。
///
/// 用 untagged + 字段互斥区分：`Message` 无 `lane_id`/`head`；`LaneHead` 无
/// `id`/`parent_id`/`message`。序列化时 `Message` 输出 009 裸形状（无 tag、与旧
/// 文件一致），`LaneHead` 输出 `{lane_id, head}`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SessionRecord {
    /// 消息节点（先尝试，绝大多数行）。
    Message(SessionEntry),
    /// lane head 元数据（后尝试）。
    LaneHead(LaneHeadRecord),
}

/// lane head 元数据持久化（可选能力，仅持久化后端实现；内存后端可 no-op/内存表）。
///
/// 复用 009 `SessionError`，不新增变体：序列化失败经 `Serde`、IO 失败经 `Io` 上抛。
#[async_trait]
pub trait LaneHeadStore: Send + Sync {
    /// append-only 追加一条 lane head 变更（后写覆盖先写，重放取最终值）。
    async fn append_lane_head(
        &self,
        lane_id: LaneId,
        head: Option<NodeId>,
    ) -> Result<(), SessionError>;

    /// 重放全量 LaneHead 记录，得 lane_id → 最终 head 表（崩溃恢复入口）。
    async fn load_lane_heads(&self) -> Result<HashMap<LaneId, Option<NodeId>>, SessionError>;
}
