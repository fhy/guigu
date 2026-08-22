use crate::core::event::AgentEvent;
use crate::core::message::Message;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{Notify, broadcast, mpsc, watch};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentSnapshot {
    pub system_prompt: String,
    pub model: Option<String>,
    pub thinking_level: crate::core::message::ThinkingLevel,
    pub messages: Vec<Arc<Message>>,
    pub is_streaming: bool,
    pub streaming_message: Option<Arc<Message>>,
    pub pending_tool_calls: std::collections::HashSet<String>,
    pub error_message: Option<String>,
}

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

#[derive(Debug, Clone)]
pub struct AgentHandle {
    tx: mpsc::Sender<AgentCommand>,
    snapshot: watch::Receiver<AgentSnapshot>,
    events: broadcast::Sender<AgentEvent>,
    #[allow(dead_code)]
    idle: Arc<Notify>,
}

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
            AgentError::Serialization(e) => {
                // 修复：使用 io 方法创建新的错误
                AgentError::Serialization(serde_json::Error::io(std::io::Error::other(
                    e.to_string(),
                )))
            }
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
        // 简化实现，实际项目中可能需要更复杂的反序列化
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

#[async_trait]
pub trait Agent: Send + Sync {
    /// 获取当前 agent 快照
    fn snapshot(&self) -> AgentSnapshot;
    /// 订阅 agent 事件
    fn subscribe(&self) -> broadcast::Receiver<AgentEvent>;
    /// 发送提示消息
    async fn prompt(&self, messages: Vec<Message>) -> Result<(), AgentError>;
    /// 继续处理
    async fn continue_(&self) -> Result<(), AgentError>;
    /// 转向指定消息
    async fn steer(&self, msg: Message) -> Result<(), AgentError>;
    /// 跟进指定消息
    async fn follow_up(&self, msg: Message) -> Result<(), AgentError>;
    /// 中止当前操作
    fn abort(&self);
    /// 等待所有操作完成
    async fn wait_for_idle(&self) -> Result<(), AgentError>;
}

#[async_trait]
impl Agent for AgentHandle {
    /// 获取当前 agent 快照
    fn snapshot(&self) -> AgentSnapshot {
        self.snapshot.borrow().clone()
    }

    /// 订阅 agent 事件
    fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.events.subscribe()
    }

    /// 发送提示消息
    async fn prompt(&self, messages: Vec<Message>) -> Result<(), AgentError> {
        self.tx.send(AgentCommand::Prompt(messages)).await?;
        Ok(())
    }

    /// 继续处理
    async fn continue_(&self) -> Result<(), AgentError> {
        self.tx.send(AgentCommand::Continue).await?;
        Ok(())
    }

    /// 转向指定消息
    async fn steer(&self, msg: Message) -> Result<(), AgentError> {
        self.tx.send(AgentCommand::Steer(msg)).await?;
        Ok(())
    }

    /// 跟进指定消息
    async fn follow_up(&self, msg: Message) -> Result<(), AgentError> {
        self.tx.send(AgentCommand::FollowUp(msg)).await?;
        Ok(())
    }

    /// 中止当前操作
    fn abort(&self) {
        // 发送 Abort 命令
        let _ = self.tx.try_send(AgentCommand::Abort);
    }

    /// 等待所有操作完成
    async fn wait_for_idle(&self) -> Result<(), AgentError> {
        // 等待所有事件处理完成
        Ok(())
    }
}

impl AgentHandle {
    /// 启动一个新的 agent 实例
    pub fn spawn(config: AgentConfig) -> Self {
        // 创建 channel
        let (tx, mut rx) = mpsc::channel::<AgentCommand>(100);
        let (snapshot_tx, snapshot_rx) = watch::channel(AgentSnapshot {
            system_prompt: config.system_prompt,
            model: config.model,
            thinking_level: config.thinking_level,
            messages: Vec::new(),
            is_streaming: false,
            streaming_message: None,
            pending_tool_calls: std::collections::HashSet::new(),
            error_message: None,
        });
        let (events_tx, _events_rx) = broadcast::channel(100);
        let idle = Arc::new(Notify::new());

        // 启动 runtime task
        let events_tx_clone = events_tx.clone();
        tokio::spawn(async move {
            // 这里是简化版本，实际实现将在后续任务中完善
            // 为本任务，我们只处理基本的命令分发
            while let Some(command) = rx.recv().await {
                match command {
                    AgentCommand::Prompt(messages) => {
                        // 发送 AgentStart 事件
                        let agent_start_event = AgentEvent::AgentStart;
                        let _ = events_tx_clone.send(agent_start_event);

                        // 发送 TurnStart 事件
                        let turn_start_event = AgentEvent::TurnStart;
                        let _ = events_tx_clone.send(turn_start_event);

                        // 添加消息到 transcript
                        let mut current_snapshot = snapshot_tx.borrow().clone();
                        let mut new_messages = current_snapshot.messages;

                        for message in &messages {
                            let arc_message = Arc::new(message.clone());
                            new_messages.push(arc_message);
                        }

                        // 更新快照
                        current_snapshot.messages = new_messages;
                        let _ = snapshot_tx.send(current_snapshot);

                        // 发送 MessageStart 事件
                        for message in &messages {
                            let message_start_event = AgentEvent::MessageStart {
                                message: Arc::new(message.clone()),
                            };
                            let _ = events_tx_clone.send(message_start_event);
                        }

                        // 发送 MessageEnd 事件
                        for message in &messages {
                            let message_end_event = AgentEvent::MessageEnd {
                                message: Arc::new(message.clone()),
                            };
                            let _ = events_tx_clone.send(message_end_event);
                        }

                        // 发送 TurnEnd 事件
                        let turn_end_event = AgentEvent::TurnEnd {
                            message: Arc::new(crate::core::message::AssistantMessage {
                                content: Vec::new(),
                                model: None,
                                usage: None,
                                stop_reason: None,
                                error_message: None,
                                timestamp: 0,
                            }),
                            tool_results: Vec::new(),
                        };
                        let _ = events_tx_clone.send(turn_end_event);

                        // 发送 AgentEnd 事件
                        let agent_end_event = AgentEvent::AgentEnd {
                            messages: messages.iter().map(|m| Arc::new(m.clone())).collect(),
                        };
                        let _ = events_tx_clone.send(agent_end_event);
                    }
                    AgentCommand::Shutdown => {
                        // 发送关闭命令
                        break;
                    }
                    _ => {
                        // 其他命令可以在这里处理
                    }
                }
            }
        });

        AgentHandle {
            tx,
            snapshot: snapshot_rx,
            events: events_tx,
            idle,
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

    /// 等待所有操作完成
    pub async fn wait_for_idle(&self) -> Result<(), AgentError> {
        // 等待所有事件处理完成
        Ok(())
    }

    /// 关闭 agent
    pub async fn shutdown(self) -> Result<(), AgentError> {
        // 发送关闭命令
        let _ = self.tx.send(AgentCommand::Shutdown).await;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentConfig {
    pub system_prompt: String,
    pub model: Option<String>,
    pub thinking_level: crate::core::message::ThinkingLevel,
}
