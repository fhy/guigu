//! Agent trait 契约与 AgentHandle（actor 外壳）。
//!
//! 并发模型：单 writer。对外 `AgentHandle`（Clone）→ `mpsc<AgentCommand>`
//! → 唯一 runtime task → `watch<AgentSnapshot>`（权威）+ `broadcast<AgentEvent>`（增量）。
//! 状态归唯一 runtime task 所有，不采用 `Arc<RwLock<AgentState>>`。
//!
//! 订阅者 broadcast lag 时须重读 snapshot，不把 broadcast 当审计/持久化通道。

use crate::core::agent_runtime::spawn_runtime;
use crate::core::event::AgentEvent;
use crate::core::message::{Message, ThinkingLevel};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::{broadcast, mpsc, watch};

/// 对外不可变的 agent 快照（watch 权威最新状态）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentSnapshot {
    pub system_prompt: String,
    pub model: Option<String>,
    pub thinking_level: ThinkingLevel,
    /// 完整 transcript 的不可变视图
    pub messages: Vec<Arc<Message>>,
    pub is_streaming: bool,
    pub streaming_message: Option<Arc<Message>>,
    pub pending_tool_calls: HashSet<String>,
    pub error_message: Option<String>,
}

/// 发往唯一 runtime task 的命令。
///
/// 并发契约：active run 期间收到的 `Prompt`/`Steer`/`FollowUp` 一律进入同一
/// FIFO 命令队列排队，待当前 run 结束后按序处理，不返回 `Busy`
/// （`AgentError::Busy` 保留为类型成员，本任务内不使用）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AgentCommand {
    Prompt(Vec<Message>),
    Continue,
    Steer(Message),
    FollowUp(Message),
    Abort,
    Reset,
    Shutdown,
}

/// Agent 句柄（Clone），命令队列 + snapshot + 订阅 + sent/processed 计数 + exited 标志。
#[derive(Debug, Clone)]
pub struct AgentHandle {
    tx: mpsc::Sender<AgentCommand>,
    snapshot: watch::Receiver<AgentSnapshot>,
    events: broadcast::Sender<AgentEvent>,
    /// 已发送命令计数（prompt/continue/steer/follow_up/reset 各 +1）。
    sent: Arc<AtomicU64>,
    /// runtime 已处理命令计数（wait_for_idle 的同步点）。
    processed: watch::Receiver<u64>,
    /// exited 标志：runtime task 退出前置 true，shutdown 据此等待。
    exited: watch::Receiver<bool>,
}

/// Agent 错误类型。
#[derive(Error, Debug)]
pub enum AgentError {
    #[error("Agent is busy")]
    Busy,
    #[error("Invalid agent state")]
    InvalidState,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Other error: {0}")]
    Other(String),
    #[error("Send error: {0}")]
    SendError(String),
}

impl Clone for AgentError {
    fn clone(&self) -> Self {
        match self {
            AgentError::Busy => AgentError::Busy,
            AgentError::InvalidState => AgentError::InvalidState,
            AgentError::Io(e) => AgentError::Io(std::io::Error::new(e.kind(), e.to_string())),
            AgentError::Serialization(e) => AgentError::Serialization(serde_json::Error::io(
                std::io::Error::other(e.to_string()),
            )),
            AgentError::Other(s) => AgentError::Other(s.clone()),
            AgentError::SendError(s) => AgentError::SendError(s.clone()),
        }
    }
}

impl Serialize for AgentError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            AgentError::Busy => serializer.serialize_str("Busy"),
            AgentError::InvalidState => serializer.serialize_str("InvalidState"),
            AgentError::Io(e) => serializer.serialize_str(&format!("Io: {}", e)),
            AgentError::Serialization(e) => {
                serializer.serialize_str(&format!("Serialization: {}", e))
            }
            AgentError::Other(s) => serializer.serialize_str(&format!("Other: {}", s)),
            AgentError::SendError(s) => serializer.serialize_str(&format!("SendError: {}", s)),
        }
    }
}

impl<'de> Deserialize<'de> for AgentError {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(AgentError::Other(s))
    }
}

impl PartialEq for AgentError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (AgentError::Busy, AgentError::Busy) => true,
            (AgentError::InvalidState, AgentError::InvalidState) => true,
            (AgentError::Io(_), AgentError::Io(_)) => true,
            (AgentError::Serialization(_), AgentError::Serialization(_)) => true,
            (AgentError::Other(s1), AgentError::Other(s2)) => s1 == s2,
            (AgentError::SendError(s1), AgentError::SendError(s2)) => s1 == s2,
            _ => false,
        }
    }
}

impl From<tokio::sync::mpsc::error::SendError<AgentCommand>> for AgentError {
    fn from(error: tokio::sync::mpsc::error::SendError<AgentCommand>) -> Self {
        AgentError::SendError(error.to_string())
    }
}

/// Agent 行为契约。
#[async_trait]
pub trait Agent: Send + Sync {
    /// 获取当前 agent 快照
    fn snapshot(&self) -> AgentSnapshot;
    /// 订阅 agent 事件
    fn subscribe(&self) -> broadcast::Receiver<AgentEvent>;
    /// 发送提示消息（入队即返回；active run 期间排队，不返回 Busy）
    async fn prompt(&self, messages: Vec<Message>) -> Result<(), AgentError>;
    /// 继续处理（入队即返回）
    async fn continue_(&self) -> Result<(), AgentError>;
    /// 转向指定消息（入队即返回）
    async fn steer(&self, msg: Message) -> Result<(), AgentError>;
    /// 跟进指定消息（入队即返回）
    async fn follow_up(&self, msg: Message) -> Result<(), AgentError>;
    /// 重置 agent（清空 transcript 与队列，入队即返回）
    async fn reset(&self) -> Result<(), AgentError>;
    /// 中止当前操作（只入队即返回，不阻塞调用方，不计入 sent）
    fn abort(&self);
    /// 等待所有已发送命令处理完成（以 processed 计数为同步点，含超时兜底）
    async fn wait_for_idle(&self) -> Result<(), AgentError>;
}

#[async_trait]
impl Agent for AgentHandle {
    fn snapshot(&self) -> AgentSnapshot {
        self.snapshot.borrow().clone()
    }

    fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.events.subscribe()
    }

    async fn prompt(&self, messages: Vec<Message>) -> Result<(), AgentError> {
        self.tx.send(AgentCommand::Prompt(messages)).await?;
        self.sent.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn continue_(&self) -> Result<(), AgentError> {
        self.tx.send(AgentCommand::Continue).await?;
        self.sent.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn steer(&self, msg: Message) -> Result<(), AgentError> {
        self.tx.send(AgentCommand::Steer(msg)).await?;
        self.sent.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn follow_up(&self, msg: Message) -> Result<(), AgentError> {
        self.tx.send(AgentCommand::FollowUp(msg)).await?;
        self.sent.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn reset(&self) -> Result<(), AgentError> {
        self.tx.send(AgentCommand::Reset).await?;
        self.sent.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn abort(&self) {
        // fire-and-forget：只入队即返回，不计入 sent（无需等待其结算）。
        let _ = self.tx.try_send(AgentCommand::Abort);
    }

    async fn wait_for_idle(&self) -> Result<(), AgentError> {
        wait_processed(&self.processed, &self.sent, "wait_for_idle").await
    }
}

impl AgentHandle {
    /// 启动唯一 runtime task，返回句柄。
    pub fn spawn(config: AgentConfig) -> Self {
        let (tx, rx) = mpsc::channel::<AgentCommand>(100);
        let (snapshot_tx, snapshot_rx) = watch::channel(AgentSnapshot {
            system_prompt: config.system_prompt.clone(),
            model: config.model.clone(),
            thinking_level: config.thinking_level.clone(),
            messages: Vec::new(),
            is_streaming: false,
            streaming_message: None,
            pending_tool_calls: HashSet::new(),
            error_message: None,
        });
        let (events_tx, _events_rx) = broadcast::channel(100);
        let (processed_tx, processed_rx) = watch::channel(0u64);
        let (exited_tx, exited_rx) = watch::channel(false);
        let sent = Arc::new(AtomicU64::new(0));

        spawn_runtime(
            rx,
            snapshot_tx,
            events_tx.clone(),
            processed_tx,
            exited_tx,
            config,
        );

        AgentHandle {
            tx,
            snapshot: snapshot_rx,
            events: events_tx,
            sent,
            processed: processed_rx,
            exited: exited_rx,
        }
    }

    /// 获取当前 agent 快照
    pub fn snapshot(&self) -> AgentSnapshot {
        self.snapshot.borrow().clone()
    }

    /// 订阅 agent 事件
    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.events.subscribe()
    }

    /// 等待所有已发送命令处理完成（带 5s 超时兜底，避免永久挂起）。
    ///
    /// 以 sent/processed 计数为同步点：`prompt` 等命令入队成功即计入 sent，
    /// runtime 处理完一条即计入 processed，故本方法会阻塞到所有已发送命令
    /// 真正处理完毕才返回；已 idle 时立即返回，同一 run 结束后可多次调用。
    pub async fn wait_for_idle(&self) -> Result<(), AgentError> {
        wait_processed(&self.processed, &self.sent, "wait_for_idle").await
    }

    /// 关闭 agent：发 Shutdown 并等 runtime task 真正退出。
    pub async fn shutdown(self) -> Result<(), AgentError> {
        let _ = self.tx.send(AgentCommand::Shutdown).await;
        wait_flag(&self.exited, "shutdown").await
    }
}

/// 等待 processed 计数追上 sent 计数，带 5s 超时兜底。
async fn wait_processed(
    processed: &watch::Receiver<u64>,
    sent: &Arc<AtomicU64>,
    ctx: &str,
) -> Result<(), AgentError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut rx = processed.clone();
    loop {
        if *rx.borrow() >= sent.load(Ordering::SeqCst) {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(AgentError::Other(format!("{ctx} timeout")));
        }
        match tokio::time::timeout(remaining, rx.changed()).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => return Err(AgentError::Other(format!("{ctx} channel closed"))),
            Err(_) => return Err(AgentError::Other(format!("{ctx} timeout"))),
        }
    }
}

/// 等待 watch 标志变为 true，带 30s 超时兜底。
async fn wait_flag(rx: &watch::Receiver<bool>, ctx: &str) -> Result<(), AgentError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut rx = rx.clone();
    loop {
        if *rx.borrow() {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(AgentError::Other(format!("{ctx} timeout")));
        }
        match tokio::time::timeout(remaining, rx.changed()).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => return Err(AgentError::Other(format!("{ctx} channel closed"))),
            Err(_) => return Err(AgentError::Other(format!("{ctx} timeout"))),
        }
    }
}

/// Agent 配置。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentConfig {
    pub system_prompt: String,
    pub model: Option<String>,
    pub thinking_level: ThinkingLevel,
}
