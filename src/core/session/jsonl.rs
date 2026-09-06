//! `JsonlSessionStorage`：append-only JSONL 持久化 + 崩溃恢复（Task 009）。
//!
//! 从 `session.rs` 拆出以控制主文件行数（conventions 体量限制）。
//! `open` 读全量恢复 `next_id`；`append` 单行原子写 + `sync_all`（进程崩溃后已返回
//! Ok 的 append 必已落盘）；`load` 逐行解析、跳过尾部半行、`reduce` 重建树。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::{
    LaneHeadRecord, LaneHeadStore, LaneId, NodeId, SessionEntry, SessionError, SessionRecord,
    SessionStorage, SessionTree, reduce_for,
};
use crate::core::message::Message;

/// append-only JSONL 会话存储。
///
/// `open` 读全量恢复 `next_id`；`append` 单行原子写 + `sync_all`（进程崩溃后已返回
/// Ok 的 append 必已落盘）；`load` 逐行解析、跳过尾部半行、`reduce` 重建树。
pub struct JsonlSessionStorage {
    path: PathBuf,
    session_id: String,
    next_id: AtomicU64,
}

impl JsonlSessionStorage {
    /// 打开（文件不存在则创建，父目录按需创建）；读全量恢复 `next_id`。
    pub async fn open(
        path: impl Into<PathBuf>,
        session_id: impl Into<String>,
    ) -> Result<Self, SessionError> {
        let path = path.into();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(parent).await?;
        }
        // 仅文件确实不存在才创建；其它 IO 错误原样传播（避免丢失既有游标状态）。
        match tokio::fs::File::open(&path).await {
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                tokio::fs::File::create(&path).await?;
            }
            Err(err) => return Err(err.into()),
        }
        // 读全量有效行，恢复续写游标：max(Message id) + 1（LaneHead 行无 id，跳过）。
        let records = Self::read_records(&path).await?;
        let max_id = records
            .iter()
            .filter_map(|record| match record {
                SessionRecord::Message(entry) => Some(entry.id),
                SessionRecord::LaneHead(_) => None,
            })
            .max();
        // max(id) == u64::MAX 时游标耗尽（不回绕为 0）。
        let next_id = match max_id {
            None => 0,
            Some(m) => m.checked_add(1).ok_or(SessionError::IdExhausted)?,
        };
        Ok(Self {
            path,
            session_id: session_id.into(),
            next_id: AtomicU64::new(next_id),
        })
    }

    /// 读全量有效行（逐行解析为 `SessionRecord`，遇首个解析失败行停止——崩溃半行
    /// 规则，与 009 `load` 一致）。文件不存在 → 空表。
    async fn read_records(path: &Path) -> Result<Vec<SessionRecord>, SessionError> {
        let mut records = Vec::new();
        match tokio::fs::File::open(path).await {
            Ok(file) => {
                let mut lines = BufReader::new(file).lines();
                while let Some(line) = lines.next_line().await? {
                    // 崩溃残留的半行（或非法行）：停止读取，忽略该行及其后。
                    let Ok(record) = serde_json::from_str::<SessionRecord>(&line) else {
                        break;
                    };
                    records.push(record);
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
        Ok(records)
    }
}

#[async_trait]
impl SessionStorage for JsonlSessionStorage {
    async fn append(
        &self,
        parent_id: Option<NodeId>,
        message: Message,
    ) -> Result<NodeId, SessionError> {
        // 原子认领下一个 id：游标达 u64::MAX 时返回 IdExhausted，绝不回绕。
        // CAS 循环保证并发下不回绕（fetch_add 在 u64::MAX 会静默回绕为 0）。
        // 注意：id 认领后若写盘失败，该 id 成为空洞（monotonic cursor 语义，允许）。
        let mut cursor = self.next_id.load(Ordering::SeqCst);
        loop {
            if cursor == u64::MAX {
                return Err(SessionError::IdExhausted);
            }
            match self.next_id.compare_exchange_weak(
                cursor,
                cursor + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(actual) => cursor = actual,
            }
        }
        let id = cursor;
        let mut line = serde_json::to_string(&SessionEntry {
            id,
            parent_id,
            message,
        })?;
        line.push('\n');
        // O_APPEND 下单次 write_all 一行是原子的（单 writer，无并发交叉）。
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        file.sync_all().await?;
        Ok(id)
    }

    async fn load(&self) -> Result<SessionTree, SessionError> {
        // 逐行解析为 `SessionRecord`，忽略 `LaneHead` 变体、只对 `Message` 变体
        // 做 `reduce`。旧文件（全 Message 行）结果与 009 完全一致。
        let records = Self::read_records(&self.path).await?;
        let entries: Vec<SessionEntry> = records
            .into_iter()
            .filter_map(|record| match record {
                SessionRecord::Message(entry) => Some(entry),
                SessionRecord::LaneHead(_) => None,
            })
            .collect();
        let tree = reduce_for(entries, &self.session_id)?;
        // 恢复续写游标：next_id = max(id) + 1（单调不减，不回退）。
        // max(id) == u64::MAX 时游标耗尽（不回绕为 0）。
        if let Some((max_id, _)) = tree.nodes.last_key_value() {
            let next = max_id.checked_add(1).ok_or(SessionError::IdExhausted)?;
            self.next_id.fetch_max(next, Ordering::SeqCst);
        }
        Ok(tree)
    }

    fn next_id(&self) -> NodeId {
        self.next_id.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LaneHeadStore for JsonlSessionStorage {
    async fn append_lane_head(
        &self,
        lane_id: LaneId,
        head: Option<NodeId>,
    ) -> Result<(), SessionError> {
        // 与 009 append 同一写路径、同一原子性保证：O_APPEND 单行 + sync_all。
        let mut line = serde_json::to_string(&LaneHeadRecord { lane_id, head })?;
        line.push('\n');
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        file.sync_all().await?;
        Ok(())
    }

    async fn load_lane_heads(&self) -> Result<HashMap<LaneId, Option<NodeId>>, SessionError> {
        // 逐行解析为 `SessionRecord`，仅收集 `LaneHead` 变体，按 lane_id 覆盖
        // （后写覆盖先写）得最终表；解析失败（半行）停止，与 009 load 同规则。
        let records = Self::read_records(&self.path).await?;
        let mut heads = HashMap::new();
        for record in records {
            if let SessionRecord::LaneHead(head) = record {
                heads.insert(head.lane_id, head.head);
            }
        }
        Ok(heads)
    }
}
