//! `JsonlSessionStorage`：append-only JSONL 持久化 + 崩溃恢复（Task 009）。
//!
//! 从 `session.rs` 拆出以控制主文件行数（conventions 体量限制）。
//! `open` 读全量恢复 `next_id`；`append` 单行原子写 + `sync_all`（进程崩溃后已返回
//! Ok 的 append 必已落盘）；`load` 逐行解析、跳过尾部半行、`reduce` 重建树。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::{NodeId, SessionEntry, SessionError, SessionStorage, SessionTree, reduce_for};
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
        let mut max_id: Option<NodeId> = None;
        match tokio::fs::File::open(&path).await {
            Ok(file) => {
                let mut lines = BufReader::new(file).lines();
                while let Some(line) = lines.next_line().await? {
                    // 崩溃残留的半行：停止读取，忽略该行及其后。
                    let Ok(entry) = serde_json::from_str::<SessionEntry>(&line) else {
                        break;
                    };
                    max_id = Some(max_id.map_or(entry.id, |m| m.max(entry.id)));
                }
            }
            // 仅文件确实不存在才创建；其它 IO 错误原样传播（避免丢失既有游标状态）。
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                tokio::fs::File::create(&path).await?;
            }
            Err(err) => return Err(err.into()),
        }
        // 恢复续写游标：max(id) + 1；max(id) == u64::MAX 时游标耗尽（不回绕为 0）。
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
        let mut entries = Vec::new();
        match tokio::fs::File::open(&self.path).await {
            Ok(file) => {
                let mut lines = BufReader::new(file).lines();
                while let Some(line) = lines.next_line().await? {
                    let Ok(entry) = serde_json::from_str::<SessionEntry>(&line) else {
                        break;
                    };
                    entries.push(entry);
                }
            }
            // 文件不存在 → 空树。
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
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
