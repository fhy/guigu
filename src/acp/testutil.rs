//! ACP 测试共享工具（Task 014）。
//!
//! 从 `tests.rs` 拆出（单测试文件 ≤ 30 个 `#[test]` 约束）：`tests` / `tests_fs` /
//! `tests_transport` 共用的 fake client、provider、storage 与 agent 构造器。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::{SinkExt, stream};
use serde_json::Value;

use crate::acp::{AcpAgent, AcpClient, AcpError};
use crate::core::agent::AgentConfig;
use crate::core::message::{
    AssistantContent, AssistantMessage, Message, StopReason, ThinkingLevel,
};
use crate::core::provider::{
    AssistantEvent, AssistantStream, Model, ModelProvider, ProviderError, ProviderRequest,
};
use crate::core::runtime::{AgentRuntime, LoopConfig};
use crate::core::session::{
    NodeId, SessionEntry, SessionError, SessionStorage, SessionTree, SharedSessionStorage, reduce,
};
use crate::server::AgentServer;

/// 内存 `SessionStorage`（测试用，避免 `JsonlSessionStorage::open` 的 async 约束）。
pub struct InMemoryStorage {
    entries: std::sync::Mutex<Vec<SessionEntry>>,
    next_id: AtomicU64,
}

impl InMemoryStorage {
    fn new() -> Self {
        Self {
            entries: std::sync::Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
        }
    }
}

#[async_trait]
impl SessionStorage for InMemoryStorage {
    async fn append(
        &self,
        parent_id: Option<NodeId>,
        message: Message,
    ) -> Result<NodeId, SessionError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.entries.lock().unwrap().push(SessionEntry {
            id,
            parent_id,
            message,
        });
        Ok(id)
    }

    async fn load(&self) -> Result<SessionTree, SessionError> {
        let entries = self.entries.lock().unwrap().clone();
        reduce(entries)
    }

    fn next_id(&self) -> NodeId {
        self.next_id.load(Ordering::SeqCst)
    }
}

/// fake `AcpClient`：记录 agent→client 调用，按方法返回预设结果。
pub struct FakeClient {
    pub calls: Arc<std::sync::Mutex<Vec<(String, Value)>>>,
    responses: Arc<std::sync::Mutex<HashMap<String, Value>>>,
}

impl FakeClient {
    pub fn new() -> Self {
        Self {
            calls: Arc::new(std::sync::Mutex::new(Vec::new())),
            responses: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    /// 预设某方法的返回结果。
    pub fn with_response(self, method: &str, response: Value) -> Self {
        self.responses
            .lock()
            .unwrap()
            .insert(method.to_string(), response);
        self
    }

    /// 某方法的全部调用参数（按序）。
    pub fn calls_with(&self, method: &str) -> Vec<Value> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p.clone())
            .collect()
    }

    /// 是否收到过某方法调用。
    pub fn has_call(&self, method: &str) -> bool {
        !self.calls_with(method).is_empty()
    }
}

#[async_trait]
impl AcpClient for FakeClient {
    async fn request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        self.calls
            .lock()
            .unwrap()
            .push((method.to_string(), params));
        Ok(self
            .responses
            .lock()
            .unwrap()
            .get(method)
            .cloned()
            .unwrap_or(Value::Null))
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), AcpError> {
        self.calls
            .lock()
            .unwrap()
            .push((method.to_string(), params));
        Ok(())
    }
}

/// 最小 provider：单文本 turn（`TextDelta` + `Done`，`stop_reason: Completed`）。
pub struct NoopProvider;

#[async_trait]
impl ModelProvider for NoopProvider {
    async fn stream(&self, _request: ProviderRequest) -> Result<AssistantStream, ProviderError> {
        let message = AssistantMessage {
            content: vec![AssistantContent::Text {
                text: "ok".to_string(),
            }],
            model: None,
            usage: None,
            stop_reason: Some(StopReason::Completed),
            error_message: None,
            timestamp: 0,
        };
        Ok(Box::pin(stream::iter(vec![
            AssistantEvent::TextDelta {
                text: "ok".to_string(),
            },
            AssistantEvent::Done { message },
        ])))
    }
}

/// 慢速 provider：每 5ms 发一个 `TextDelta`，检查取消信号（供 cancel 测试）。
///
/// 用 `futures::channel::mpsc`（`Receiver` 实现 `Stream`）承载事件流；后台 task
/// 周期性发事件（使 `stream_turn` 的 `drain_commands` 能处理 `Abort`），检查取消
/// 信号，取消时发 `Error { aborted: true }` 并停止。
pub struct SlowProvider;

#[async_trait]
impl ModelProvider for SlowProvider {
    async fn stream(&self, request: ProviderRequest) -> Result<AssistantStream, ProviderError> {
        let signal = request.signal.clone();
        let (mut tx, rx) = futures::channel::mpsc::channel::<AssistantEvent>(16);
        tokio::spawn(async move {
            for i in 0..100_000 {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(5)) => {}
                    _ = signal.cancelled() => {
                        let _ = tx
                            .send(AssistantEvent::Error {
                                message: "aborted".into(),
                                aborted: true,
                            })
                            .await;
                        return;
                    }
                }
                if tx
                    .send(AssistantEvent::TextDelta {
                        text: format!("c{i}"),
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        Ok(Box::pin(rx))
    }
}

/// 建一个配置好 runtime / storage 工厂的 `AcpAgent`。
pub fn make_agent(provider: Arc<dyn ModelProvider>) -> AcpAgent {
    let server = AgentServer::new();
    server.with_runtime_factory(move || {
        (
            AgentConfig {
                system_prompt: "test".to_string(),
                model: Some("test-model".to_string()),
                thinking_level: ThinkingLevel::Off,
            },
            AgentRuntime {
                provider: provider.clone(),
                tools: Vec::new(),
                loop_config: LoopConfig {
                    model: Model {
                        id: "test-model".to_string(),
                        context_window: 8192,
                    },
                    ..LoopConfig::default()
                },
            },
        )
    });
    server.with_storage_factory(|_id| {
        Arc::new(SharedSessionStorage::new(Arc::new(InMemoryStorage::new())))
    });
    AcpAgent::new(server)
}
