//! Session 树 + JSONL 崩溃恢复（Task 009）。
//!
//! - `SessionTree` / `SessionNode` / `SessionEntry`：树结构；fork = 向任意历史节点追加
//! - `SessionStorage`：trait（落定 architecture 3.8 预留接口）
//! - `JsonlSessionStorage`：append-only JSONL 持久化 + 崩溃恢复
//! - `reduce`：纯函数重放 entries 重建树（结构校验集中于此）
//! - `SessionRecorder`：把 001 事件流桥接到存储（单 lane 游标）
//!
//! 边界声明：单 writer / 单进程 / 单 agent；`sync_all` 保证进程崩溃（`kill -9`）级
//! 持久性，不保证断电级；多 lane 并发写属 010。

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::broadcast,
};

use crate::core::{event::AgentEvent, message::Message};

/// 节点 id（单调递增，由存储分配）。
pub type NodeId = u64;

/// 会话树（reducer 产物，内存态）。
#[derive(Debug, Clone, PartialEq)]
pub struct SessionTree {
    /// 会话 id（由存储赋予；独立调用 `reduce` 时为空串）。
    pub session_id: String,
    /// 根节点 id（单根）。
    pub root: Option<NodeId>,
    /// 全部节点（按 id 索引）。
    pub nodes: BTreeMap<NodeId, SessionNode>,
}

/// 会话树中的单个节点。
#[derive(Debug, Clone, PartialEq)]
pub struct SessionNode {
    /// 节点 id。
    pub id: NodeId,
    /// 父节点 id；`None` = 根。
    pub parent_id: Option<NodeId>,
    /// 本节点承载的消息。
    pub message: Message,
    /// 直接子节点（reducer 填充，冗余便于遍历；按 id 升序）。
    pub children: Vec<NodeId>,
}

impl SessionTree {
    /// 叶子节点集合（`children` 为空 = 各活跃分支头）。
    pub fn leaves(&self) -> Vec<NodeId> {
        self.nodes
            .values()
            .filter(|node| node.children.is_empty())
            .map(|node| node.id)
            .collect()
    }

    /// 从根到某叶的线性消息序列（用于恢复 transcript）。
    ///
    /// 仅定义于叶节点：传入不存在的 id 或**非叶节点**（`children` 非空）均返回
    /// `None`，调用方据此区分完整 transcript 与中间路径，避免误恢复非活跃分支。
    pub fn path_to(&self, leaf: NodeId) -> Option<Vec<&Message>> {
        let node = self.nodes.get(&leaf)?;
        if !node.children.is_empty() {
            return None;
        }
        let mut path = Vec::new();
        let mut cursor = Some(leaf);
        while let Some(id) = cursor {
            let node = self.nodes.get(&id)?;
            path.push(&node.message);
            cursor = node.parent_id;
        }
        path.reverse();
        Some(path)
    }
}

/// JSONL 一行（append 的序列化单元）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEntry {
    /// 节点 id。
    pub id: NodeId,
    /// 父节点 id；fork：指向任意历史节点 id。
    pub parent_id: Option<NodeId>,
    /// 本节点承载的消息。
    pub message: Message,
}

/// 会话存储错误。
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// IO 错误。
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// 序列化错误。
    #[error("serialize error: {0}")]
    Serde(#[from] serde_json::Error),
    /// 重复节点 id。
    #[error("duplicate node id: {0}")]
    DuplicateNode(NodeId),
    /// 父节点不存在。
    #[error("parent node not found: {0}")]
    ParentNotFound(NodeId),
    /// 不允许多根。
    #[error("multiple roots not allowed")]
    MultipleRoots,
    /// 检测到环。
    #[error("cycle detected")]
    Cycle,
    /// 节点 id 游标耗尽（已达 `u64::MAX`，无法分配新 id）。
    #[error("node id cursor exhausted")]
    IdExhausted,
}

/// 会话存储（落定 architecture 3.8 预留接口）。
#[async_trait]
pub trait SessionStorage: Send + Sync {
    /// 追加一条消息为新节点，返回新节点 id。
    ///
    /// O(1) append-only：不校验 parent 是否存在（结构校验集中在 `load`/`reduce`）。
    async fn append(
        &self,
        parent_id: Option<NodeId>,
        message: Message,
    ) -> Result<NodeId, SessionError>;

    /// 读全量 entries + reduce 重建树（崩溃恢复入口）。
    ///
    /// 文件不存在时返回空树；成功后同时恢复续写游标（`next_id = max(id) + 1`，单调不减）。
    async fn load(&self) -> Result<SessionTree, SessionError>;

    /// 下一个待分配 id（崩溃恢复后必须恢复到 `max(id) + 1`，保证续写不重复）。
    fn next_id(&self) -> NodeId;
}

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

/// 重放 entries 重建树（纯函数，崩溃恢复核心）。
///
/// 校验规则（按序）：id 唯一 → 至多一个根 → parent 存在（全局校验，不依赖 entry
/// 顺序）→ 无环（显式父链遍历）→ 按 id 升序填充 `children`（顺序稳定）。
pub fn reduce(entries: Vec<SessionEntry>) -> Result<SessionTree, SessionError> {
    reduce_for(entries, "")
}

fn reduce_for(entries: Vec<SessionEntry>, session_id: &str) -> Result<SessionTree, SessionError> {
    let mut nodes: BTreeMap<NodeId, SessionNode> = BTreeMap::new();
    let mut root: Option<NodeId> = None;
    for entry in entries {
        if nodes.contains_key(&entry.id) {
            return Err(SessionError::DuplicateNode(entry.id));
        }
        if entry.parent_id.is_none() {
            if root.is_some() {
                return Err(SessionError::MultipleRoots);
            }
            root = Some(entry.id);
        }
        nodes.insert(
            entry.id,
            SessionNode {
                id: entry.id,
                parent_id: entry.parent_id,
                message: entry.message,
                children: Vec::new(),
            },
        );
    }
    for node in nodes.values() {
        if let Some(parent) = node.parent_id
            && !nodes.contains_key(&parent)
        {
            return Err(SessionError::ParentNotFound(parent));
        }
    }
    check_acyclic(&nodes)?;
    // 先收集 (parent, child) 边再填充，避免 values() 不可变借用与 get_mut 冲突。
    // BTreeMap 按 id 升序迭代 → 每个 parent 的 children 按子 id 升序填充（顺序稳定）。
    let edges: Vec<(NodeId, NodeId)> = nodes
        .values()
        .filter_map(|node| node.parent_id.map(|parent| (parent, node.id)))
        .collect();
    for (parent, child) in edges {
        if let Some(parent_node) = nodes.get_mut(&parent) {
            parent_node.children.push(child);
        }
    }
    Ok(SessionTree {
        session_id: session_id.to_string(),
        root,
        nodes,
    })
}

/// 显式无环检查：沿父链三色遍历（0 未访问 / 1 进行中 / 2 已验证到根）。
///
/// 调用方须已完成 parent 存在性校验（保证 `nodes[&id]` 安全）。
fn check_acyclic(nodes: &BTreeMap<NodeId, SessionNode>) -> Result<(), SessionError> {
    const IN_PROGRESS: u8 = 1;
    const DONE: u8 = 2;
    let mut state: BTreeMap<NodeId, u8> = BTreeMap::new();
    for &start in nodes.keys() {
        if state.get(&start).copied().unwrap_or(0) == DONE {
            continue;
        }
        let mut path = Vec::new();
        let mut cursor: Option<NodeId> = Some(start);
        while let Some(id) = cursor {
            match state.get(&id).copied().unwrap_or(0) {
                IN_PROGRESS => return Err(SessionError::Cycle),
                DONE => break,
                _ => {
                    state.insert(id, IN_PROGRESS);
                    path.push(id);
                    cursor = nodes[&id].parent_id;
                }
            }
        }
        for id in path {
            state.insert(id, DONE);
        }
    }
    Ok(())
}

/// 会话记录器：把 001 事件流桥接到 `SessionStorage`（单 lane 游标）。
///
/// `record` 串行追加（事件顺序 = 写盘顺序）；`fork_at` 显式设定 fork 点；
/// `attach` 从 broadcast 流逐条消费 `MessageEnd`（其它事件忽略）。
pub struct SessionRecorder {
    storage: Arc<dyn SessionStorage>,
    head: Option<NodeId>,
}

impl SessionRecorder {
    /// 创建记录器（初始 head 为 `None`，首次 `record` 成为根）。
    pub fn new(storage: Arc<dyn SessionStorage>) -> Self {
        Self {
            storage,
            head: None,
        }
    }

    /// 把一条消息挂到当前 head 之后并推进游标，返回新节点 id。
    pub async fn record(&mut self, message: Message) -> Result<NodeId, SessionError> {
        let id = self.storage.append(self.head, message).await?;
        self.head = Some(id);
        Ok(id)
    }

    /// 从某历史节点 fork：后续 `record` 挂到该节点之后。
    pub fn fork_at(&mut self, parent: NodeId) {
        self.head = Some(parent);
    }

    /// 接入 001 事件流：对 `MessageEnd` 逐条串行 record；其它事件忽略；
    /// `Lagged` 跳过并告警；通道关闭（所有 sender 释放）时返回。
    pub async fn attach(&mut self, mut rx: broadcast::Receiver<AgentEvent>) {
        loop {
            match rx.recv().await {
                Ok(AgentEvent::MessageEnd { message }) => {
                    if let Err(err) = self.record((*message).clone()).await {
                        tracing::warn!("session: record failed: {err}");
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!("session: recorder lagged, skipped {skipped} events");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }
}

#[cfg(test)]
mod tests;
