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
use crate::core::file_lock::{FileLock, FileLockGuard};
use crate::core::message::Message;

/// append-only JSONL 会话存储。
///
/// `open` 读全量恢复 `next_id`；`append` 单行原子写 + `sync_all`（进程崩溃后已返回
/// Ok 的 append 必已落盘）；`load` 逐行解析、跳过尾部半行、`reduce` 重建树。
///
/// Task 028：可选叠加跨进程锁（[`JsonlSessionStorage::open_locked`]）。启用后
/// `append` / `append_lane_head` 在落盘前获取跨进程独占锁，实现同 session 文件
/// 跨进程 append 串行（不交错半行）。`load` 不抢锁（对齐 012 既有约定）。
pub struct JsonlSessionStorage {
    path: PathBuf,
    session_id: String,
    next_id: AtomicU64,
    /// Task 028：跨进程锁（仅 `open_locked` 启用时 `Some`）。
    file_lock: Option<FileLock>,
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
            file_lock: None,
        })
    }

    /// 打开并启用跨进程锁（Task 028）：`append` / `append_lane_head` 落盘前获取
    /// 跨进程独占锁，实现同 session 文件跨进程 append 串行（不交错半行）。
    ///
    /// `lock` 通常由 `FileLock::for_path(&path)` 构造（锁文件 =
    /// `<session.jsonl>.guigu.lock`）。`load` 不抢锁（对齐 012 既有约定）。
    ///
    /// 零破坏：`open` 默认无跨进程锁，行为与 009 一致。
    pub async fn open_locked(
        path: impl Into<PathBuf>,
        session_id: impl Into<String>,
        lock: FileLock,
    ) -> Result<Self, SessionError> {
        let mut storage = Self::open(path, session_id).await?;
        storage.file_lock = Some(lock);
        Ok(storage)
    }

    /// Task 028：获取跨进程锁（仅当启用时）。返回 `None` 表示未启用跨进程锁。
    async fn acquire_file_lock(&self) -> Result<Option<FileLockGuard>, SessionError> {
        match &self.file_lock {
            Some(lock) => {
                let guard = lock
                    .lock_exclusive()
                    .await
                    .map_err(|e| SessionError::Io(std::io::Error::other(e.to_string())))?;
                Ok(Some(guard))
            }
            None => Ok(None),
        }
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
        // Task 028：跨进程锁（仅当启用时）。在 id 认领之后、落盘之前获取，guard
        // Drop 解锁（覆盖写盘失败/取消提前返回）。
        let _file_guard = self.acquire_file_lock().await?;
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
        // Task 028：跨进程锁（仅当启用时），与 `append` 同一锁文件，保证
        // message 行与 lane head 行跨进程不交错。
        let _file_guard = self.acquire_file_lock().await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::message::{Message, UserContent, UserMessage};
    use std::sync::Arc;

    fn user_msg(text: &str) -> Message {
        Message::User(UserMessage {
            content: vec![UserContent::Text { text: text.into() }],
            timestamp: 0,
        })
    }

    /// Task 028：open_locked 启用跨进程锁，append 可正常获取锁并落盘。
    #[tokio::test]
    async fn test_open_locked_append() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session.jsonl");
        let lock = FileLock::for_path(&path);
        let storage = JsonlSessionStorage::open_locked(&path, "test-session", lock)
            .await
            .expect("open_locked should succeed");

        let id = storage
            .append(None, user_msg("hello"))
            .await
            .expect("append should succeed");
        assert_eq!(id, 0, "first append should get id 0");

        // load 验证落盘（load 不抢锁）。
        let tree = storage.load().await.expect("load should succeed");
        assert_eq!(tree.nodes.len(), 1, "should have 1 node");
    }

    /// Task 028：open_locked 顺序 append 正确（跨进程锁每次获取/释放）。
    #[tokio::test]
    async fn test_open_locked_sequential_append() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session.jsonl");
        let lock = FileLock::for_path(&path);
        let storage = Arc::new(
            JsonlSessionStorage::open_locked(&path, "test-session", lock)
                .await
                .expect("open_locked should succeed"),
        );

        // 顺序 append 8 条（parent 链），验证每次 append 正确获取/释放跨进程锁。
        let mut parent: Option<u64> = None;
        for i in 0..8 {
            let msg = user_msg(&format!("msg-{i}"));
            let id = storage
                .append(parent, msg)
                .await
                .expect("append should succeed");
            parent = Some(id);
        }

        // load 验证条数正确、无半行、树结构正确。
        let tree = storage.load().await.expect("load should succeed");
        assert_eq!(tree.nodes.len(), 8, "should have 8 nodes");
        assert_eq!(tree.root, Some(0), "root should be id 0");
    }

    /// 零破坏：open 默认无跨进程锁，行为与 009 一致。
    #[tokio::test]
    async fn test_open_default_no_file_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session.jsonl");
        let storage = JsonlSessionStorage::open(&path, "test-session")
            .await
            .expect("open should succeed");

        let id = storage
            .append(None, user_msg("hello"))
            .await
            .expect("append should succeed");
        assert_eq!(id, 0, "first append should get id 0");
    }
}
