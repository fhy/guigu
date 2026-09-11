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
    ///
    /// 锁错误经 `SessionError::FileLock` 原样传播（保留 `FileLockError` 类型与
    /// source 链），不降级为普通 IO 错误。
    async fn acquire_file_lock(&self) -> Result<Option<FileLockGuard>, SessionError> {
        match &self.file_lock {
            Some(lock) => Ok(Some(lock.lock_exclusive().await?)),
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

    /// 认领下一个 id（CAS）：游标达 `u64::MAX` 时返回 `IdExhausted`，绝不回绕。
    ///
    /// CAS 循环保证并发下不回绕（`fetch_add` 在 `u64::MAX` 会静默回绕为 0）。
    /// 注意：id 认领后若写盘失败，该 id 成为空洞（monotonic cursor 语义，允许）。
    fn claim_next_id(&self) -> Result<NodeId, SessionError> {
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
                Ok(_) => return Ok(cursor),
                Err(actual) => cursor = actual,
            }
        }
    }

    /// 锁内刷新续写游标（Task 028）：调用方须已持跨进程锁。重读文件得跨进程权威
    /// max id，`fetch_max` 刷新进程内游标（地板）；文件尾部若为崩溃残留半行/非法
    /// 行，截断到最后一个完整行（持锁独占，截断安全）。
    ///
    /// 使锁内 append 成为完整事务：「读游标 / 分配 ID / 写入 / sync_all」全程在锁
    /// 内，多进程写同一 session 文件时 id 唯一（不重复）。
    async fn refresh_cursor_locked(&self) -> Result<(), SessionError> {
        let (records, valid_len) = Self::read_records_with_extent(&self.path).await?;
        // 崩溃半行修复：文件实际长度 > 有效前缀 → 截断（持锁，无并发写）。
        let meta = tokio::fs::metadata(&self.path).await?;
        if meta.len() > valid_len {
            let file = tokio::fs::OpenOptions::new()
                .write(true)
                .open(&self.path)
                .await?;
            file.set_len(valid_len).await?;
        }
        let max_id = records
            .iter()
            .filter_map(|record| match record {
                SessionRecord::Message(entry) => Some(entry.id),
                SessionRecord::LaneHead(_) => None,
            })
            .max();
        // max(id) == u64::MAX 时游标耗尽（不回绕为 0）。
        let floor = match max_id {
            None => 0,
            Some(m) => m.checked_add(1).ok_or(SessionError::IdExhausted)?,
        };
        self.next_id.fetch_max(floor, Ordering::SeqCst);
        Ok(())
    }

    /// 读全量有效行 + 有效前缀字节长度（Task 028，锁内崩溃修复用）。
    ///
    /// 逐行解析为 `SessionRecord`，遇首个解析失败行停止（崩溃半行规则，与 009
    /// `load` 一致）。返回 `(records, valid_len)`：`valid_len` 为所有成功解析行（含
    /// 换行符）的字节总长，即「最后一个完整行」的结束偏移。文件实际长度 >
    /// `valid_len` 时，尾部为崩溃残留半行/非法行，调用方（持锁）应截断到
    /// `valid_len`。
    async fn read_records_with_extent(
        path: &Path,
    ) -> Result<(Vec<SessionRecord>, u64), SessionError> {
        let file = match tokio::fs::File::open(path).await {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), 0)),
            Err(err) => return Err(err.into()),
        };
        let mut reader = BufReader::new(file);
        let mut records = Vec::new();
        let mut valid_len: u64 = 0;
        loop {
            let mut line = String::new();
            let bytes_read = reader.read_line(&mut line).await?;
            if bytes_read == 0 {
                break; // EOF
            }
            // line 含尾部换行（若有）；解析前 trim 掉换行符。
            let trimmed = line.trim_end_matches(['\n', '\r']);
            match serde_json::from_str::<SessionRecord>(trimmed) {
                Ok(record) => {
                    records.push(record);
                    valid_len += bytes_read as u64; // 含换行符
                }
                Err(_) => break, // 解析失败：停止（崩溃半行规则）
            }
        }
        Ok((records, valid_len))
    }
}

#[async_trait]
impl SessionStorage for JsonlSessionStorage {
    async fn append(
        &self,
        parent_id: Option<NodeId>,
        message: Message,
    ) -> Result<NodeId, SessionError> {
        // Task 028：跨进程锁启用时，锁必须覆盖「读游标 / 分配 ID / 写入 / sync_all」
        // 整个事务——先抢锁，再在锁内重读文件刷新游标（跨进程权威 max id）并修复
        // 崩溃半行，然后认领 id 落盘。未启用时保持 009 行为（进程内原子认领，O(1)）。
        // `file_guard` 存活至函数结束（晚于写文件句柄 drop），故锁覆盖整个写入；
        // 提前返回（错误）时立即 drop 释放锁。
        let file_guard = self.acquire_file_lock().await?;
        if file_guard.is_some() {
            self.refresh_cursor_locked().await?;
        }
        let id = self.claim_next_id()?;
        let mut line = serde_json::to_string(&SessionEntry {
            id,
            parent_id,
            message,
        })?;
        line.push('\n');
        // O_APPEND 下单次 write_all 一行是原子的（跨进程经锁串行、进程内经锁/单
        // writer，无并发交叉）。
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
mod tests;
