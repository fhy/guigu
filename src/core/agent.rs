use crate::core::event::AgentEvent;
use crate::core::message::Message;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, watch, Notify};

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
    idle: Arc<Notify>,
}

impl AgentHandle {
    pub fn spawn(_config: AgentConfig) -> Self {
        // 这里是简化版本，实际实现将在后续任务中完善
        todo!("Implementation will be done in task 003")
    }

    pub fn snapshot(&self) -> AgentSnapshot {
        self.snapshot.borrow().clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.events.subscribe()
    }

    pub async fn wait_for_idle(&self) -> Result<(), AgentError> {
        // 等待所有事件处理完成
        Ok(())
    }

    pub async fn shutdown(self) -> Result<(), AgentError> {
        // 发送关闭命令
        Ok(())
    }
}

// 简化版的 Agent trait
pub trait Agent {
    fn snapshot(&self) -> AgentSnapshot;
    fn subscribe(&self) -> broadcast::Receiver<AgentEvent>;
    fn prompt(&self, messages: Vec<Message>) -> Result<(), AgentError>;
    fn continue_(&self) -> Result<(), AgentError>;
    fn steer(&self, msg: Message) -> Result<(), AgentError>;
    fn follow_up(&self, msg: Message) -> Result<(), AgentError>;
    fn abort(&self);
    fn wait_for_idle(&self) -> Result<(), AgentError>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentConfig {
    pub system_prompt: String,
    pub model: Option<String>,
    pub thinking_level: crate::core::message::ThinkingLevel,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentError {
    pub message: String,
    pub kind: AgentErrorKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AgentErrorKind {
    Busy,
    InvalidState,
    Io,
    Other,
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AgentError {}

impl From<tokio::sync::oneshot::error::RecvError> for AgentError {
    fn from(error: tokio::sync::oneshot::error::RecvError) -> Self {
        AgentError {
            message: error.to_string(),
            kind: AgentErrorKind::Other,
        }
    }
}

impl From<std::io::Error> for AgentError {
    fn from(error: std::io::Error) -> Self {
        AgentError {
            message: error.to_string(),
            kind: AgentErrorKind::Io,
        }
    }
}

impl From<serde_json::Error> for AgentError {
    fn from(error: serde_json::Error) -> Self {
        AgentError {
            message: error.to_string(),
            kind: AgentErrorKind::Other,
        }
    }
}

// InMemoryAgent 实现（最小内存实现）
pub struct InMemoryAgent {
    snapshot: watch::Sender<AgentSnapshot>,
    events: broadcast::Sender<AgentEvent>,
    idle: Arc<Notify>,
    is_running: bool,
}

impl InMemoryAgent {
    pub fn new(config: AgentConfig) -> Self {
        let snapshot = AgentSnapshot {
            system_prompt: config.system_prompt,
            model: config.model,
            thinking_level: config.thinking_level,
            messages: Vec::new(),
            is_streaming: false,
            streaming_message: None,
            pending_tool_calls: std::collections::HashSet::new(),
            error_message: None,
        };

        let (snapshot_tx, _snapshot_rx) = watch::channel(snapshot);
        let (events_tx, _events_rx) = broadcast::channel(100);
        let idle = Arc::new(Notify::new());

        InMemoryAgent {
            snapshot: snapshot_tx,
            events: events_tx,
            idle,
            is_running: false,
        }
    }

    pub fn snapshot(&self) -> AgentSnapshot {
        self.snapshot.borrow().clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.events.subscribe()
    }

    pub fn prompt(&mut self, messages: Vec<Message>) -> Result<(), AgentError> {
        // 检查是否正在运行
        if self.is_running {
            return Err(AgentError {
                message: "Agent is busy".to_string(),
                kind: AgentErrorKind::Busy,
            });
        }

        // 标记为运行中
        self.is_running = true;

        // 发送 AgentStart 事件
        let agent_start_event = AgentEvent::AgentStart;
        let _ = self.events.send(agent_start_event);

        // 发送 TurnStart 事件
        let turn_start_event = AgentEvent::TurnStart;
        let _ = self.events.send(turn_start_event);

        // 添加消息到 transcript
        let mut current_snapshot = self.snapshot.borrow().clone();
        let mut new_messages = current_snapshot.messages;
        
        for message in &messages {
            let arc_message = Arc::new(message.clone());
            new_messages.push(arc_message);
        }

        // 更新快照
        current_snapshot.messages = new_messages;
        let _ = self.snapshot.send(current_snapshot);

        // 发送 MessageStart 事件
        for message in &messages {
            let message_start_event = AgentEvent::MessageStart {
                message: Arc::new(message.clone()),
            };
            let _ = self.events.send(message_start_event);
        }

        // 发送 MessageEnd 事件
        for message in &messages {
            let message_end_event = AgentEvent::MessageEnd {
                message: Arc::new(message.clone()),
            };
            let _ = self.events.send(message_end_event);
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
        let _ = self.events.send(turn_end_event);

        // 发送 AgentEnd 事件
        let agent_end_event = AgentEvent::AgentEnd {
            messages: messages.iter().map(|m| Arc::new(m.clone())).collect(),
        };
        let _ = self.events.send(agent_end_event);

        // 标记为非运行中
        self.is_running = false;

        // 通知等待的线程
        self.idle.notify_waiters();

        Ok(())
    }

    pub fn continue_(&self) -> Result<(), AgentError> {
        // 简化实现，实际应处理继续逻辑
        Ok(())
    }

    pub fn steer(&self, _msg: Message) -> Result<(), AgentError> {
        // 简化实现，实际应处理转向逻辑
        Ok(())
    }

    pub fn follow_up(&self, _msg: Message) -> Result<(), AgentError> {
        // 简化实现，实际应处理后续逻辑
        Ok(())
    }

    pub fn abort(&self) {
        // 简化实现，实际应处理中止逻辑
    }

    pub fn wait_for_idle(&self) -> Result<(), AgentError> {
        // 等待所有事件处理完成
        Ok(())
    }
}