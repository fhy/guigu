//! ACP 适配集成测试（Task 014）：stdio loopback（duplex + fake client）。
//!
//! 完整 ACP 会话往返：initialize → session/new → session/prompt → session/update
//! 逐条推送 → 最终 stopReason 正确。用 `tokio::io::duplex` 模拟 stdio，
//! `AcpAgent::serve_connection` 跑 agent 侧，测试代码跑 client 侧。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use serde_json::{Value, json};

use guigu::acp::AcpAgent;
use guigu::core::agent::AgentConfig;
use guigu::core::message::{
    AssistantContent, AssistantMessage, Message, StopReason, ThinkingLevel,
};
use guigu::core::provider::{
    AssistantEvent, AssistantStream, Model, ModelProvider, ProviderError, ProviderRequest,
};
use guigu::core::runtime::{AgentRuntime, LoopConfig};
use guigu::core::session::{
    NodeId, SessionEntry, SessionError, SessionStorage, SessionTree, SharedSessionStorage, reduce,
};
use guigu::remote::codec::{LineReader, write_line};
use guigu::server::AgentServer;

/// 最小 provider：单文本 turn（`TextDelta` + `Done`，`stop_reason: Completed`）。
struct NoopProvider;

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

/// 内存 `SessionStorage`（测试用）。
struct InMemoryStorage {
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

/// 建一个配置好 runtime / storage 工厂的 `AcpAgent`。
fn make_agent() -> AcpAgent {
    let server = AgentServer::new();
    server.with_runtime_factory(|| {
        (
            AgentConfig {
                system_prompt: "test".to_string(),
                model: Some("test-model".to_string()),
                thinking_level: ThinkingLevel::Off,
            },
            AgentRuntime {
                provider: Arc::new(NoopProvider),
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

/// stdio loopback 集成测试：完整 ACP 会话往返。
///
/// initialize → session/new → session/prompt → session/update 逐条推送 →
/// 最终 stopReason 正确（`end_turn`）。
#[tokio::test]
async fn test_stdio_loopback_full_session() {
    let agent = make_agent();

    // 设置 duplex：client 侧（写请求 / 读响应）+ agent 侧（读请求 / 写响应）。
    let (agent_side, client_side) = tokio::io::duplex(4096);
    let (agent_reader, agent_writer) = tokio::io::split(agent_side);
    let (client_reader, mut client_writer) = tokio::io::split(client_side);

    // 启动 agent 侧（serve_connection）。
    let agent_task = tokio::spawn(async move {
        agent
            .serve_connection(agent_reader, agent_writer)
            .await
            .expect("serve_connection")
    });

    let mut reader = LineReader::new(client_reader);

    // 1. initialize。
    write_line(
        &mut client_writer,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": 1 }
        }),
    )
    .await
    .expect("write initialize");
    let init_resp: Value = reader.next().await.expect("read").expect("some");
    assert_eq!(init_resp["result"]["protocolVersion"], 1);
    assert_eq!(
        init_resp["result"]["agentCapabilities"]["loadSession"],
        true
    );
    assert_eq!(
        init_resp["result"]["authMethods"].as_array().unwrap().len(),
        0
    );

    // 2. session/new。
    write_line(
        &mut client_writer,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": { "cwd": "/tmp" }
        }),
    )
    .await
    .expect("write session/new");
    let new_resp: Value = reader.next().await.expect("read").expect("some");
    let session_id = new_resp["result"]["sessionId"]
        .as_str()
        .expect("sessionId")
        .to_string();

    // 3. session/prompt（双工：run 进行中逐条推 session/update notification）。
    write_line(
        &mut client_writer,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": "hi" }]
            }
        }),
    )
    .await
    .expect("write session/prompt");

    // 读消息直到 prompt 应答（id=3）到达；沿途收集 session/update notification。
    let mut updates: Vec<Value> = Vec::new();
    let mut prompt_resp: Option<Value> = None;
    while prompt_resp.is_none() {
        let msg: Value = tokio::time::timeout(Duration::from_secs(10), reader.next())
            .await
            .expect("read should not time out")
            .expect("read")
            .expect("some");
        if msg.get("method").and_then(Value::as_str) == Some("session/update") {
            updates.push(msg);
        } else if msg.get("id").and_then(Value::as_u64) == Some(3) {
            prompt_resp = Some(msg);
        }
    }

    // 事件逐条推：至少一条 agent_message_chunk（NoopProvider 发一个 TextDelta）。
    assert!(
        !updates.is_empty(),
        "should receive session/update notifications"
    );
    let has_text_chunk = updates.iter().any(|u| {
        u["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
            && u["params"]["update"]["content"]["text"] == "ok"
    });
    assert!(
        has_text_chunk,
        "should have agent_message_chunk with text 'ok'"
    );
    // 每条 update 携带 sessionId。
    for u in &updates {
        assert_eq!(u["params"]["sessionId"], session_id);
    }

    // 最终 stopReason 正确（Completed → end_turn）。
    let resp = prompt_resp.expect("prompt response");
    assert_eq!(resp["result"]["stopReason"], "end_turn");

    // 关闭 client 侧 → agent 侧读到 EOF 退出。
    drop(client_writer);
    drop(reader);
    agent_task.await.expect("agent task should finish");
}
