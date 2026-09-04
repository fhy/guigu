//! Agent Server（Task 013）：多 client / 多 session / 多 lane 调度核心。
//!
//! - `AgentServer`：session 注册表 + lane 调度门面（transport 无关）
//! - `SessionState` / `LaneRuntime`：session 与 lane 的运行时状态
//! - `ServerError`：server 错误类型
//!
//! 并发策略：注册表用 `tokio::sync::Mutex`（写多、粒度粗、保简单）；不跨 await
//! 持锁——`spawn_lane` / `fork_lane` 先 spawn 再入表，`load_session` 先 load 再入表。
//!
//! 契约复用（不改既有签名）：001 `AgentHandle` / 003 `AgentRuntime` / 009
//! `SessionStorage` / 012 `SharedSessionStorage` + `LaneWriter`。
//!
//! 边界声明：认证 / TLS / 鉴权不做（同 010）；跨进程多写者不做（009/012 已声明）；
//! ACP 属 014。

mod lane;
mod protocol;
mod transport;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicU64;

use tokio::sync::Mutex;

use crate::core::agent::{AgentConfig, AgentHandle};
use crate::core::runtime::AgentRuntime;
use crate::core::session::{LaneId, LaneWriter, SessionError, SessionStorage};

pub use protocol::{ServerMessage, ServerRequest};

/// session 标识。
pub type SessionId = String;

/// runtime 工厂：transport 的 `SpawnLane` / `ForkLane` 用它构造 `(config, runtime)`。
pub type RuntimeFactory = Arc<dyn Fn() -> (AgentConfig, AgentRuntime) + Send + Sync>;
/// storage 工厂：transport 的 `CreateSession` 用它构造 session 存储。
pub type StorageFactory = Arc<dyn Fn(&str) -> Arc<dyn SessionStorage> + Send + Sync>;

/// server 错误类型。
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// session 不存在。
    #[error("session not found: {0}")]
    SessionNotFound(SessionId),
    /// lane 不存在。
    #[error("lane not found: {0}")]
    LaneNotFound(LaneId),
    /// lane 已存在。
    #[error("lane already exists: {0}")]
    LaneAlreadyExists(LaneId),
    /// session 重复。
    #[error("duplicate session: {0}")]
    DuplicateSession(SessionId),
    /// session 存储错误。
    #[error("session error: {0}")]
    Session(#[from] SessionError),
    /// agent 错误（`AgentError` 字符串化）。
    #[error("agent error: {0}")]
    Agent(String),
    /// IO 错误。
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// 协议层错误。
    #[error("protocol error: {0}")]
    Protocol(String),
}

/// 一个 lane 的运行时：一个活跃 `AgentHandle`（一个 runtime task）+ 持久化写游标。
///
/// `writer` 与桥接 task 共享（`Arc<Mutex<LaneWriter>>`）：桥接 task 串行落盘
/// `MessageEnd`，`fork_lane` 读其 `head` 作为分支点。`bridge` 是桥接 task 的
/// `JoinHandle`：`shutdown` 等其完成，保证进程退出前持久化落盘。
struct LaneRuntime {
    /// lane 标识（与注册表 key 冗余，保留供日志 / 调试）。
    #[allow(dead_code)]
    lane_id: LaneId,
    handle: AgentHandle,
    writer: Arc<Mutex<LaneWriter>>,
    bridge: tokio::task::JoinHandle<()>,
}

/// 一个 session 的运行时状态：存储 + 活跃 lane 集合。
struct SessionState {
    /// session 标识（与注册表 key 冗余，保留供日志 / 调试）。
    #[allow(dead_code)]
    session_id: SessionId,
    storage: Arc<dyn SessionStorage>,
    lanes: HashMap<LaneId, LaneRuntime>,
}

struct ServerInner {
    sessions: Mutex<HashMap<SessionId, SessionState>>,
    runtime_factory: OnceLock<RuntimeFactory>,
    storage_factory: OnceLock<StorageFactory>,
    next_session_id: AtomicU64,
}

/// Agent Server 门面：多 session 注册表 + 多 lane 调度（transport 无关）。
#[derive(Clone)]
pub struct AgentServer {
    inner: Arc<ServerInner>,
}

impl Default for AgentServer {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentServer {
    /// 创建空 server（无 session）。
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ServerInner {
                sessions: Mutex::new(HashMap::new()),
                runtime_factory: OnceLock::new(),
                storage_factory: OnceLock::new(),
                next_session_id: AtomicU64::new(1),
            }),
        }
    }

    /// 设置 runtime 工厂（transport 的 `SpawnLane` / `ForkLane` 用它构造 runtime）。
    ///
    /// 仅首次生效（`OnceLock`）；重复调用忽略。
    pub fn with_runtime_factory(
        &self,
        factory: impl Fn() -> (AgentConfig, AgentRuntime) + Send + Sync + 'static,
    ) {
        let _ = self.inner.runtime_factory.set(Arc::new(factory));
    }

    /// 设置 storage 工厂（transport 的 `CreateSession` 用它构造 session 存储）。
    ///
    /// 仅首次生效（`OnceLock`）；重复调用忽略。
    pub fn with_storage_factory(
        &self,
        factory: impl Fn(&str) -> Arc<dyn SessionStorage> + Send + Sync + 'static,
    ) {
        let _ = self.inner.storage_factory.set(Arc::new(factory));
    }

    /// 新建空 session；`session_id` 已存在于注册表 → `DuplicateSession`。
    pub async fn create_session(
        &self,
        session_id: SessionId,
        storage: Arc<dyn SessionStorage>,
    ) -> Result<(), ServerError> {
        let mut sessions = self.inner.sessions.lock().await;
        if sessions.contains_key(&session_id) {
            return Err(ServerError::DuplicateSession(session_id));
        }
        sessions.insert(
            session_id.clone(),
            SessionState {
                session_id,
                storage,
                lanes: HashMap::new(),
            },
        );
        Ok(())
    }

    /// 从持久化存储 load + reduce 重建 session 并注册（崩溃恢复入口）。
    ///
    /// `load` 在锁外执行（恢复续写游标），再入表；`session_id` 已存在 →
    /// `DuplicateSession`。
    pub async fn load_session(
        &self,
        session_id: SessionId,
        storage: Arc<dyn SessionStorage>,
    ) -> Result<(), ServerError> {
        // load 在锁外执行（避免跨 await 持锁）；成功后恢复续写游标。
        storage.load().await?;
        let mut sessions = self.inner.sessions.lock().await;
        if sessions.contains_key(&session_id) {
            return Err(ServerError::DuplicateSession(session_id));
        }
        sessions.insert(
            session_id.clone(),
            SessionState {
                session_id,
                storage,
                lanes: HashMap::new(),
            },
        );
        Ok(())
    }

    /// 列出全部 session id（按字典序）。
    pub async fn list_sessions(&self) -> Vec<SessionId> {
        let sessions = self.inner.sessions.lock().await;
        let mut ids: Vec<SessionId> = sessions.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// 分配一个单调递增的 session id（供 ACP 等 transport 生成 `sessionId`）。
    pub fn allocate_session_id(&self) -> SessionId {
        self.inner
            .next_session_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .to_string()
    }

    /// 用已配置的 storage 工厂新建 session（工厂未配置 → `Protocol` 错误）。
    pub async fn create_session_from_factory(
        &self,
        session_id: SessionId,
    ) -> Result<(), ServerError> {
        let factory = self
            .inner
            .storage_factory
            .get()
            .cloned()
            .ok_or_else(|| ServerError::Protocol("no storage factory".into()))?;
        let storage = factory(&session_id);
        self.create_session(session_id, storage).await
    }

    /// 用已配置的 storage 工厂 load + 重建 session（工厂未配置 → `Protocol` 错误）。
    pub async fn load_session_from_factory(
        &self,
        session_id: SessionId,
    ) -> Result<(), ServerError> {
        let factory = self
            .inner
            .storage_factory
            .get()
            .cloned()
            .ok_or_else(|| ServerError::Protocol("no storage factory".into()))?;
        let storage = factory(&session_id);
        self.load_session(session_id, storage).await
    }

    /// 用已配置的 runtime 工厂在 session 内 spawn 一个 lane（工厂未配置 → `Protocol` 错误）。
    pub async fn spawn_lane_from_factory(
        &self,
        session_id: &str,
        lane_id: &str,
    ) -> Result<(), ServerError> {
        let factory = self
            .inner
            .runtime_factory
            .get()
            .cloned()
            .ok_or_else(|| ServerError::Protocol("no runtime factory".into()))?;
        let (config, runtime) = factory();
        self.spawn_lane(session_id, lane_id, config, runtime).await
    }
}

#[cfg(test)]
mod tests;
