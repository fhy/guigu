//! ACP 适配单测（Task 014）。
//!
//! 覆盖：方法映射（initialize / session/new / session/prompt / session/cancel /
//! set_mode）、事件映射（AgentEvent → SessionUpdate）、stopReason 映射、`AcpFsTool`
//! （fs 读写经 client 代理 + 权限判定）。用 fake `AcpClient` 断言 agent→client 调用，
//! 用 `NoopProvider` / `SlowProvider` 驱动真实 runtime 事件流（`tokio::test` 真跑）。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::{SinkExt, stream};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::acp::mapping::{acp_stop_reason, content_blocks_to_messages, map_event_to_update};
use crate::acp::transport::{
    InboundKind, InboundMessage, OutboundMessage, RequestId, StdioClient, classify_inbound,
};
use crate::acp::types::{AcpStopReason, ContentBlock, PermissionOutcome};
use crate::acp::{AcpAgent, AcpClient, AcpError, AcpFsTool, PermissionMode};
use crate::core::agent::AgentConfig;
use crate::core::event::AgentEvent;
use crate::core::message::{
    AssistantContent, AssistantMessage, Message, StopReason, ThinkingLevel,
};
use crate::core::provider::{
    AssistantEvent, AssistantStream, Model, ModelProvider, ProviderError, ProviderRequest,
};
use crate::core::runtime::{AgentRuntime, LoopConfig};
use crate::core::session::{
    NodeId, SessionEntry, SessionError, SessionStorage, SessionTree, reduce,
};
use crate::core::tool::{Tool, ToolError, ToolResult};
use crate::remote::codec::LineReader;
use crate::server::AgentServer;

/// 内存 `SessionStorage`（测试用，避免 `JsonlSessionStorage::open` 的 async 约束）。
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

/// fake `AcpClient`：记录 agent→client 调用，按方法返回预设结果。
struct FakeClient {
    calls: Arc<std::sync::Mutex<Vec<(String, Value)>>>,
    responses: Arc<std::sync::Mutex<HashMap<String, Value>>>,
}

impl FakeClient {
    fn new() -> Self {
        Self {
            calls: Arc::new(std::sync::Mutex::new(Vec::new())),
            responses: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    /// 预设某方法的返回结果。
    fn with_response(self, method: &str, response: Value) -> Self {
        self.responses
            .lock()
            .unwrap()
            .insert(method.to_string(), response);
        self
    }

    /// 某方法的全部调用参数（按序）。
    fn calls_with(&self, method: &str) -> Vec<Value> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p.clone())
            .collect()
    }

    /// 是否收到过某方法调用。
    fn has_call(&self, method: &str) -> bool {
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

/// 慢速 provider：每 5ms 发一个 `TextDelta`，检查取消信号（供 cancel 测试）。
///
/// 用 `futures::channel::mpsc`（`Receiver` 实现 `Stream`）承载事件流；后台 task
/// 周期性发事件（使 `stream_turn` 的 `drain_commands` 能处理 `Abort`），检查取消
/// 信号，取消时发 `Error { aborted: true }` 并停止。
struct SlowProvider;

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
fn make_agent(provider: Arc<dyn ModelProvider>) -> AcpAgent {
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
    server.with_storage_factory(|_id| Arc::new(InMemoryStorage::new()));
    AcpAgent::new(server)
}

/// `initialize` 返回合法 `AgentCapabilities`（`loadSession: true`、`authMethods: []`）。
#[tokio::test]
async fn test_initialize_returns_capabilities() {
    let agent = make_agent(Arc::new(NoopProvider));
    let client = FakeClient::new();
    let result = agent
        .handle(&client, "initialize", json!({"protocolVersion": 1}))
        .await
        .expect("initialize");
    assert_eq!(result["protocolVersion"], 1);
    assert_eq!(result["agentCapabilities"]["loadSession"], true);
    assert_eq!(
        result["agentCapabilities"]["promptCapabilities"]["image"],
        false
    );
    assert_eq!(result["authMethods"].as_array().unwrap().len(), 0);
    assert_eq!(result["agentInfo"]["name"], "guigu");
}

/// `session/new` 返回分配的 `sessionId`。
#[tokio::test]
async fn test_session_new_returns_session_id() {
    let agent = make_agent(Arc::new(NoopProvider));
    let client = FakeClient::new();
    let result = agent
        .handle(&client, "session/new", json!({"cwd": "/tmp"}))
        .await
        .expect("session/new");
    let session_id = result["sessionId"].as_str().expect("sessionId");
    assert!(!session_id.is_empty(), "sessionId should not be empty");
}

/// `session/load` 从持久化恢复并返回 `{ sessionId }`（对齐任务规格方法映射表）。
#[tokio::test]
async fn test_session_load_returns_session_id() {
    let agent = make_agent(Arc::new(NoopProvider));
    let client = FakeClient::new();
    let result = agent
        .handle(&client, "session/load", json!({ "sessionId": "s1" }))
        .await
        .expect("session/load");
    assert_eq!(result["sessionId"], "s1");
}

/// `session/prompt` 收到 `session/update` 序列并返回 `PromptResponse.stopReason`。
#[tokio::test]
async fn test_session_prompt_returns_stop_reason() {
    let agent = make_agent(Arc::new(NoopProvider));
    let client = FakeClient::new();

    let new_result = agent
        .handle(&client, "session/new", json!({}))
        .await
        .expect("session/new");
    let session_id = new_result["sessionId"].as_str().unwrap().to_string();

    let result = agent
        .handle(
            &client,
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": "hi" }]
            }),
        )
        .await
        .expect("session/prompt");
    assert_eq!(result["stopReason"], "end_turn");

    // 应收到 agent_message_chunk 推送（NoopProvider 发一个 TextDelta）。
    let updates = client.calls_with("session/update");
    assert!(!updates.is_empty(), "should receive session/update");
    let has_text_chunk = updates.iter().any(|u| {
        u["update"]["sessionUpdate"] == "agent_message_chunk"
            && u["update"]["content"]["text"] == "ok"
    });
    assert!(has_text_chunk, "should have agent_message_chunk with text");
}

/// `session/cancel` 触发 lane abort（prompt 返回 `stopReason: cancelled`）。
#[tokio::test]
async fn test_session_cancel_aborts_lane() {
    let agent = Arc::new(make_agent(Arc::new(SlowProvider)));
    let client = Arc::new(FakeClient::new());

    let new_result = agent
        .handle(&*client, "session/new", json!({}))
        .await
        .expect("session/new");
    let session_id = new_result["sessionId"].as_str().unwrap().to_string();

    // 后台跑 prompt（SlowProvider 持续发事件）。
    let agent_clone = Arc::clone(&agent);
    let client_clone = Arc::clone(&client);
    let session_id_clone = session_id.clone();
    let prompt_task = tokio::spawn(async move {
        agent_clone
            .handle(
                &*client_clone,
                "session/prompt",
                json!({
                    "sessionId": session_id_clone,
                    "prompt": [{ "type": "text", "text": "hi" }]
                }),
            )
            .await
    });

    // 等 run 启动（收到 session/update）。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if client.has_call("session/update") {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("run should have started");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // 发 cancel。
    agent
        .handle(
            &*client,
            "session/cancel",
            json!({ "sessionId": session_id }),
        )
        .await
        .expect("session/cancel");

    let result = prompt_task.await.expect("prompt task");
    let result = result.expect("prompt should succeed");
    assert_eq!(result["stopReason"], "cancelled");
}

/// `session/set_mode` 更新权限模式。
#[tokio::test]
async fn test_set_mode_updates_permission() {
    let agent = make_agent(Arc::new(NoopProvider));
    let client = FakeClient::new();

    let new_result = agent
        .handle(&client, "session/new", json!({}))
        .await
        .expect("session/new");
    let session_id = new_result["sessionId"].as_str().unwrap().to_string();

    agent
        .handle(
            &client,
            "session/set_mode",
            json!({ "sessionId": session_id, "modeId": "plan" }),
        )
        .await
        .expect("set_mode");

    let mode = *agent.mode_for(&session_id).await.read().await;
    assert_eq!(mode, PermissionMode::Plan);
}

/// `authenticate` 一期不支持 → 返回 `authMethods: []`（对齐任务规格，非 JSON-RPC 错误）。
#[tokio::test]
async fn test_authenticate_returns_empty_auth_methods() {
    let agent = make_agent(Arc::new(NoopProvider));
    let client = FakeClient::new();
    let result = agent
        .handle(&client, "authenticate", json!({ "methodId": "token" }))
        .await
        .expect("authenticate should succeed (not error)");
    assert_eq!(result["authMethods"].as_array().unwrap().len(), 0);
}

/// 多 session 权限隔离：session A 设 `plan` 不影响 session B（仍 `default`）。
#[tokio::test]
async fn test_set_mode_session_isolation() {
    let agent = make_agent(Arc::new(NoopProvider));
    let client = FakeClient::new();

    let a = agent
        .handle(&client, "session/new", json!({}))
        .await
        .expect("new A");
    let a_id = a["sessionId"].as_str().unwrap().to_string();
    let b = agent
        .handle(&client, "session/new", json!({}))
        .await
        .expect("new B");
    let b_id = b["sessionId"].as_str().unwrap().to_string();

    // session A 设 plan。
    agent
        .handle(
            &client,
            "session/set_mode",
            json!({ "sessionId": a_id, "modeId": "plan" }),
        )
        .await
        .expect("set A");

    // A 是 plan，B 仍是 default（隔离，未串改）。
    let a_mode = *agent.mode_for(&a_id).await.read().await;
    let b_mode = *agent.mode_for(&b_id).await.read().await;
    assert_eq!(a_mode, PermissionMode::Plan);
    assert_eq!(b_mode, PermissionMode::Default);
}

/// `set_mode` 对不存在的 session 返回错误（不修改任何权限状态）。
#[tokio::test]
async fn test_set_mode_unknown_session_errors() {
    let agent = make_agent(Arc::new(NoopProvider));
    let client = FakeClient::new();
    let result = agent
        .handle(
            &client,
            "session/set_mode",
            json!({ "sessionId": "nonexistent", "modeId": "plan" }),
        )
        .await;
    assert!(result.is_err(), "should error for unknown session");
    // 不存在的 session 不应在 modes 表中留下任何状态。
    let mode = *agent.mode_for("nonexistent").await.read().await;
    assert_eq!(
        mode,
        PermissionMode::Default,
        "unknown session mode untouched"
    );
}

/// `set_mode` 缺 `sessionId` → 错误（session 级操作必须携带 sessionId）。
#[tokio::test]
async fn test_set_mode_missing_session_id_errors() {
    let agent = make_agent(Arc::new(NoopProvider));
    let client = FakeClient::new();
    let result = agent
        .handle(&client, "session/set_mode", json!({ "modeId": "plan" }))
        .await;
    assert!(result.is_err(), "should error when sessionId missing");
}

/// 事件映射：`TextDelta` → `agent_message_chunk`。
#[test]
fn test_event_mapping_text() {
    let event = AgentEvent::MessageUpdate {
        message: Arc::new(Message::Assistant(AssistantMessage {
            content: vec![],
            model: None,
            usage: None,
            stop_reason: None,
            error_message: None,
            timestamp: 0,
        })),
        assistant_event: AssistantEvent::TextDelta {
            text: "hello".to_string(),
        },
    };
    let update = map_event_to_update(&event).expect("should map");
    assert_eq!(update["sessionUpdate"], "agent_message_chunk");
    assert_eq!(update["content"]["type"], "text");
    assert_eq!(update["content"]["text"], "hello");
}

/// 事件映射：`ThinkingDelta` → `agent_thought_chunk`。
#[test]
fn test_event_mapping_thinking() {
    let event = AgentEvent::MessageUpdate {
        message: Arc::new(Message::Assistant(AssistantMessage {
            content: vec![],
            model: None,
            usage: None,
            stop_reason: None,
            error_message: None,
            timestamp: 0,
        })),
        assistant_event: AssistantEvent::ThinkingDelta {
            thinking: "hmm".to_string(),
        },
    };
    let update = map_event_to_update(&event).expect("should map");
    assert_eq!(update["sessionUpdate"], "agent_thought_chunk");
    assert_eq!(update["content"]["text"], "hmm");
}

/// 事件映射：`ToolExecutionStart` → `tool_call`（status pending）。
#[test]
fn test_event_mapping_tool_call() {
    let event = AgentEvent::ToolExecutionStart {
        tool_call_id: "tc1".to_string(),
        tool_name: "read".to_string(),
        args: json!({ "path": "/tmp/x" }),
    };
    let update = map_event_to_update(&event).expect("should map");
    assert_eq!(update["sessionUpdate"], "tool_call");
    assert_eq!(update["toolCallId"], "tc1");
    assert_eq!(update["kind"], "read");
    assert_eq!(update["status"], "pending");
}

/// 事件映射：`ToolExecutionEnd` → `tool_call_update`（status completed/failed）。
#[test]
fn test_event_mapping_tool_result() {
    let result = ToolResult::text("file content");
    let event = AgentEvent::ToolExecutionEnd {
        tool_call_id: "tc1".to_string(),
        tool_name: "read".to_string(),
        result,
        is_error: false,
    };
    let update = map_event_to_update(&event).expect("should map");
    assert_eq!(update["sessionUpdate"], "tool_call_update");
    assert_eq!(update["toolCallId"], "tc1");
    assert_eq!(update["status"], "completed");
    assert_eq!(update["content"][0]["content"]["text"], "file content");

    // is_error → failed。
    let result = ToolResult::error("boom");
    let event = AgentEvent::ToolExecutionEnd {
        tool_call_id: "tc2".to_string(),
        tool_name: "read".to_string(),
        result,
        is_error: true,
    };
    let update = map_event_to_update(&event).expect("should map");
    assert_eq!(update["status"], "failed");
}

/// 非推送事件（`AgentStart` / `TurnStart` 等）→ `None`。
#[test]
fn test_event_mapping_no_push() {
    assert!(map_event_to_update(&AgentEvent::AgentStart).is_none());
    assert!(map_event_to_update(&AgentEvent::TurnStart).is_none());
    assert!(map_event_to_update(&AgentEvent::AgentEnd { messages: vec![] }).is_none());
}

/// stopReason 映射：各 `StopReason` → ACP `stopReason`。
#[test]
fn test_stop_reason_mapping() {
    assert_eq!(
        acp_stop_reason(&StopReason::Completed),
        AcpStopReason::EndTurn
    );
    assert_eq!(
        acp_stop_reason(&StopReason::Length),
        AcpStopReason::MaxTokens
    );
    assert_eq!(acp_stop_reason(&StopReason::Error), AcpStopReason::Refusal);
    assert_eq!(
        acp_stop_reason(&StopReason::Aborted),
        AcpStopReason::Cancelled
    );
    assert_eq!(
        acp_stop_reason(&StopReason::Pending),
        AcpStopReason::EndTurn
    );
    assert_eq!(
        acp_stop_reason(&StopReason::Other("x".into())),
        AcpStopReason::EndTurn
    );
}

/// `ContentBlock[]` → guigu `Vec<Message>`（文本合并为一条 UserMessage）。
#[test]
fn test_content_blocks_to_messages() {
    let blocks = vec![
        ContentBlock::Text {
            text: "hello".to_string(),
        },
        ContentBlock::Text {
            text: "world".to_string(),
        },
    ];
    let messages = content_blocks_to_messages(&blocks);
    assert_eq!(messages.len(), 1);
    match &messages[0] {
        Message::User(u) => assert_eq!(u.content.len(), 2),
        _ => panic!("expected User message"),
    }
    // 空块 → 空 Vec。
    assert!(content_blocks_to_messages(&[]).is_empty());
}

/// `PermissionOutcome` 解析：selected / cancelled。
#[test]
fn test_permission_outcome_parsing() {
    let selected = PermissionOutcome::from_value(&json!({
        "outcome": { "outcome": "selected", "optionId": "allow_once" }
    }));
    assert!(selected.allowed());

    let cancelled = PermissionOutcome::from_value(&json!({
        "outcome": { "outcome": "cancelled" }
    }));
    assert!(!cancelled.allowed());

    // 非法 → Cancelled。
    let invalid = PermissionOutcome::from_value(&json!({}));
    assert!(!invalid.allowed());
}

/// `AcpFsTool` 读：经 client 代理（`fs/read_text_file`），bypassPermissions 不请求权限。
#[tokio::test]
async fn test_fs_tool_read() {
    let fake = Arc::new(
        FakeClient::new().with_response("fs/read_text_file", json!({ "content": "file content" })),
    );
    let client: Arc<dyn AcpClient> = fake.clone();
    let mode = Arc::new(tokio::sync::RwLock::new(PermissionMode::BypassPermissions));
    let tool = AcpFsTool::new(client, "s1".to_string(), mode);

    let result = tool
        .execute(
            "tc1",
            json!({ "operation": "read", "path": "/tmp/x" }),
            CancellationToken::new(),
            None,
        )
        .await
        .expect("read");
    assert!(!result.is_error);
    assert!(
        fake.has_call("fs/read_text_file"),
        "should call fs/read_text_file"
    );
    assert!(
        !fake.has_call("session/request_permission"),
        "bypass should not request permission"
    );
    // 结果含文件内容。
    match &result.content[0] {
        crate::core::message::ToolResultContent::Text { text } => {
            assert_eq!(text, "file content")
        }
        _ => panic!("expected text content"),
    }
}

/// `AcpFsTool` 写：经 client 代理（`fs/write_text_file`）。
#[tokio::test]
async fn test_fs_tool_write() {
    let fake = Arc::new(FakeClient::new());
    let client: Arc<dyn AcpClient> = fake.clone();
    let mode = Arc::new(tokio::sync::RwLock::new(PermissionMode::BypassPermissions));
    let tool = AcpFsTool::new(client, "s1".to_string(), mode);

    let result = tool
        .execute(
            "tc1",
            json!({ "operation": "write", "path": "/tmp/x", "content": "data" }),
            CancellationToken::new(),
            None,
        )
        .await
        .expect("write");
    assert!(!result.is_error);
    assert!(
        fake.has_call("fs/write_text_file"),
        "should call fs/write_text_file"
    );
    let write_params = fake.calls_with("fs/write_text_file");
    assert_eq!(write_params[0]["content"], "data");
    assert_eq!(write_params[0]["path"], "/tmp/x");
}

/// `AcpFsTool` 权限：mode=plan 时先发 `session/request_permission`，授权后才写。
#[tokio::test]
async fn test_fs_tool_permission_plan() {
    let fake = Arc::new(FakeClient::new().with_response(
        "session/request_permission",
        json!({ "outcome": { "outcome": "selected", "optionId": "allow_once" } }),
    ));
    let client: Arc<dyn AcpClient> = fake.clone();
    let mode = Arc::new(tokio::sync::RwLock::new(PermissionMode::Plan));
    let tool = AcpFsTool::new(client, "s1".to_string(), mode);

    let result = tool
        .execute(
            "tc1",
            json!({ "operation": "write", "path": "/tmp/x", "content": "data" }),
            CancellationToken::new(),
            None,
        )
        .await
        .expect("write");
    assert!(!result.is_error);
    // 先请求权限，再写文件。
    assert!(
        fake.has_call("session/request_permission"),
        "should request permission"
    );
    assert!(
        fake.has_call("fs/write_text_file"),
        "should write after permission"
    );
    let calls = fake.calls.lock().unwrap();
    let perm_idx = calls
        .iter()
        .position(|(m, _)| m == "session/request_permission")
        .expect("perm call");
    let write_idx = calls
        .iter()
        .position(|(m, _)| m == "fs/write_text_file")
        .expect("write call");
    assert!(perm_idx < write_idx, "permission should come before write");
}

/// `AcpFsTool` 权限：mode=plan 且 client 拒绝 → 不写文件，返回错误结果。
#[tokio::test]
async fn test_fs_tool_permission_denied() {
    let fake = Arc::new(FakeClient::new().with_response(
        "session/request_permission",
        json!({ "outcome": { "outcome": "cancelled" } }),
    ));
    let client: Arc<dyn AcpClient> = fake.clone();
    let mode = Arc::new(tokio::sync::RwLock::new(PermissionMode::Plan));
    let tool = AcpFsTool::new(client, "s1".to_string(), mode);

    let result = tool
        .execute(
            "tc1",
            json!({ "operation": "write", "path": "/tmp/x", "content": "data" }),
            CancellationToken::new(),
            None,
        )
        .await
        .expect("write");
    assert!(result.is_error, "should be error when denied");
    assert!(
        !fake.has_call("fs/write_text_file"),
        "should not write when denied"
    );
}

/// `AcpFsTool` 未知操作 → `ToolError`。
#[tokio::test]
async fn test_fs_tool_unknown_operation() {
    let fake = Arc::new(FakeClient::new());
    let client: Arc<dyn AcpClient> = fake.clone();
    let mode = Arc::new(tokio::sync::RwLock::new(PermissionMode::BypassPermissions));
    let tool = AcpFsTool::new(client, "s1".to_string(), mode);

    let result = tool
        .execute(
            "tc1",
            json!({ "operation": "delete", "path": "/tmp/x" }),
            CancellationToken::new(),
            None,
        )
        .await;
    match result {
        Err(ToolError { message }) => assert!(message.contains("unknown operation")),
        _ => panic!("expected ToolError for unknown operation"),
    }
}

// ===== JSON-RPC 分帧（transport）=====

/// `OutboundMessage` 应答（成功）序列化：含 `id` + `result`，无 `method` / `error`。
#[test]
fn test_outbound_result_serialization() {
    let msg = OutboundMessage::result(
        Value::from(1),
        serde_json::json!({"stopReason": "end_turn"}),
    );
    let json = serde_json::to_string(&msg).expect("serialize");
    let v: Value = serde_json::from_str(&json).expect("parse");
    assert_eq!(v["id"], 1);
    assert_eq!(v["result"]["stopReason"], "end_turn");
    assert!(v.get("method").is_none(), "result should not have method");
    assert!(v.get("error").is_none(), "result should not have error");
}

/// `OutboundMessage` 应答（错误）序列化：含 `id` + `error`，无 `result`。
#[test]
fn test_outbound_error_serialization() {
    let msg = OutboundMessage::error(Value::from(2), -32603, "boom".into());
    let json = serde_json::to_string(&msg).expect("serialize");
    let v: Value = serde_json::from_str(&json).expect("parse");
    assert_eq!(v["id"], 2);
    assert_eq!(v["error"]["code"], -32603);
    assert_eq!(v["error"]["message"], "boom");
    assert!(v.get("result").is_none(), "error should not have result");
}

/// `OutboundMessage` notification 序列化：无 `id`，有 `method` + `params`。
#[test]
fn test_outbound_notification_serialization() {
    let msg = OutboundMessage::notification(
        "session/update",
        serde_json::json!({ "sessionId": "s1", "update": {} }),
    );
    let json = serde_json::to_string(&msg).expect("serialize");
    let v: Value = serde_json::from_str(&json).expect("parse");
    assert!(v.get("id").is_none(), "notification should not have id");
    assert_eq!(v["method"], "session/update");
    assert_eq!(v["params"]["sessionId"], "s1");
}

/// `InboundMessage` 请求反序列化：有 `method` + `id` + `params`。
#[test]
fn test_inbound_request_deserialization() {
    let json = r#"{"jsonrpc":"2.0","id":1,"method":"session/new","params":{"cwd":"/tmp"}}"#;
    let msg: InboundMessage = serde_json::from_str(json).expect("parse");
    assert_eq!(msg.method.as_deref(), Some("session/new"));
    assert_eq!(msg.id, Some(Value::from(1)));
    assert_eq!(msg.params.as_ref().unwrap()["cwd"], "/tmp");
    assert!(msg.result.is_none());
    assert!(msg.error.is_none());
}

/// `InboundMessage` notification 反序列化：有 `method`，无 `id`。
#[test]
fn test_inbound_notification_deserialization() {
    let json = r#"{"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s1"}}"#;
    let msg: InboundMessage = serde_json::from_str(json).expect("parse");
    assert_eq!(msg.method.as_deref(), Some("session/cancel"));
    assert!(msg.id.is_none(), "notification should not have id");
}

/// `InboundMessage` 应答反序列化：无 `method`，有 `id` + `result`。
#[test]
fn test_inbound_response_deserialization() {
    let json = r#"{"jsonrpc":"2.0","id":7,"result":{"content":"hello"}}"#;
    let msg: InboundMessage = serde_json::from_str(json).expect("parse");
    assert!(msg.method.is_none(), "response should not have method");
    assert_eq!(msg.id, Some(Value::from(7)));
    assert_eq!(msg.result.as_ref().unwrap()["content"], "hello");
}

/// `InboundMessage` 应答（错误）反序列化：有 `id` + `error`。
#[test]
fn test_inbound_error_response_deserialization() {
    let json = r#"{"jsonrpc":"2.0","id":8,"error":{"code":-32601,"message":"not found"}}"#;
    let msg: InboundMessage = serde_json::from_str(json).expect("parse");
    assert!(msg.method.is_none());
    assert_eq!(msg.id, Some(Value::from(8)));
    assert_eq!(msg.error.as_ref().unwrap().code, -32601);
    assert_eq!(msg.error.as_ref().unwrap().message, "not found");
}

/// JSON-RPC 分帧 roundtrip：`OutboundMessage` 编码后经 `LineReader` 解码还原。
#[tokio::test]
async fn test_jsonrpc_framing_roundtrip() {
    use tokio::io::duplex;

    let (mut client, server) = duplex(4096);
    let mut reader = LineReader::new(server);

    let msgs = vec![
        OutboundMessage::result(Value::from(1), serde_json::json!({"sessionId": "s1"})),
        OutboundMessage::notification(
            "session/update",
            serde_json::json!({ "sessionId": "s1", "update": { "sessionUpdate": "agent_message_chunk" } }),
        ),
        OutboundMessage::error(Value::from(2), -32603, "boom".into()),
    ];
    // 合并为一次写入（模拟多帧到达同一 read buffer）。
    let mut buf = Vec::new();
    for m in &msgs {
        let mut bytes = serde_json::to_vec(m).expect("encode");
        bytes.push(b'\n');
        buf.extend(bytes);
    }
    use tokio::io::AsyncWriteExt;
    client.write_all(&buf).await.expect("write");
    client.flush().await.expect("flush");

    for _ in &msgs {
        let decoded = reader
            .next::<InboundMessage>()
            .await
            .expect("read")
            .expect("some");
        assert!(decoded.id.is_some() || decoded.method.is_some());
    }
}

// ===== RequestId（Issue 2：string / 负数 id 路由）=====

/// `RequestId::from_value`：string / 正数 / 负数合法；bool / null / 对象 → `None`。
#[test]
fn test_request_id_from_value() {
    assert_eq!(
        RequestId::from_value(&Value::from("abc")),
        Some(RequestId::String("abc".into()))
    );
    assert_eq!(
        RequestId::from_value(&Value::from(42)),
        Some(RequestId::Number(42))
    );
    assert_eq!(
        RequestId::from_value(&Value::from(-7)),
        Some(RequestId::Number(-7))
    );
    // 非法类型 → None。
    assert_eq!(RequestId::from_value(&Value::Bool(true)), None);
    assert_eq!(RequestId::from_value(&Value::Null), None);
    assert_eq!(RequestId::from_value(&json!({})), None);
    assert_eq!(RequestId::from_value(&json!([1])), None);
}

/// `RequestId::to_value` 往返：string / 负数 id 序列化后与 `from_value` 一致。
#[test]
fn test_request_id_roundtrip() {
    for id in [
        RequestId::String("req-1".into()),
        RequestId::Number(0),
        RequestId::Number(-1),
        RequestId::Number(i64::MAX),
    ] {
        let v = id.to_value();
        assert_eq!(
            RequestId::from_value(&v),
            Some(id.clone()),
            "roundtrip {id:?}"
        );
    }
}

/// 应答路由：字符串 id / 负数 id 都能正确唤醒对应 pending（Issue 2）。
#[tokio::test]
async fn test_resolve_pending_string_and_negative_id() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OutboundMessage>();
    let client = StdioClient::new(tx);

    // 字符串 id。
    let s_rx = client
        .insert_pending_for_test(RequestId::String("abc".into()))
        .await;
    client
        .resolve_pending(RequestId::String("abc".into()), Ok(json!({ "ok": true })))
        .await;
    let result = s_rx.await.expect("string id should resolve").expect("ok");
    assert_eq!(result["ok"], true);

    // 负数 id。
    let n_rx = client.insert_pending_for_test(RequestId::Number(-5)).await;
    client
        .resolve_pending(RequestId::Number(-5), Ok(json!({ "neg": true })))
        .await;
    let result = n_rx.await.expect("negative id should resolve").expect("ok");
    assert_eq!(result["neg"], true);

    // 不匹配的 id 不应误唤醒（pending 已清空）。
    assert_eq!(client.pending_len().await, 0);
}

// ===== pending 清理（Issue 4 / Issue 5）=====

/// writer 已关闭时 `request` 失败，且 pending entry 被清理（无泄漏，Issue 4）。
#[tokio::test]
async fn test_pending_cleanup_on_send_failure() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<OutboundMessage>();
    let client = StdioClient::new(tx);
    drop(rx); // 关闭 writer 接收端，使 send 失败。

    let result = client.request("fs/read_text_file", json!({})).await;
    assert!(result.is_err(), "should error when writer closed");
    assert_eq!(
        client.pending_len().await,
        0,
        "pending should be cleaned up"
    );
}

/// `cancel_all`：连接断开时全部 pending 以明确错误结束并清空（Issue 5）。
#[tokio::test]
async fn test_cancel_all_resolves_pending() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OutboundMessage>();
    let client = StdioClient::new(tx);

    let rx1 = client.insert_pending_for_test(RequestId::Number(1)).await;
    let rx2 = client
        .insert_pending_for_test(RequestId::String("in-flight".into()))
        .await;
    assert_eq!(client.pending_len().await, 2);

    client.cancel_all().await;
    assert_eq!(client.pending_len().await, 0, "pending should be cleared");

    // 两个等待中的请求都以「连接关闭」错误结束（而非永久挂起）。
    let e1 = rx1
        .await
        .expect("rx1 resolved")
        .expect_err("should be cancelled");
    assert!(e1.to_string().contains("connection closed"));
    let e2 = rx2
        .await
        .expect("rx2 resolved")
        .expect_err("should be cancelled");
    assert!(e2.to_string().contains("connection closed"));
}

// ===== ContentBlock 不支持类型（建议 3）=====

/// `session/prompt` 含非文本块（image）→ 明确「不支持内容类型」错误（非泛化 serde 错误）。
#[tokio::test]
async fn test_prompt_unsupported_content_type() {
    let agent = make_agent(Arc::new(NoopProvider));
    let client = FakeClient::new();

    let new_result = agent
        .handle(&client, "session/new", json!({}))
        .await
        .expect("session/new");
    let session_id = new_result["sessionId"].as_str().unwrap().to_string();

    let result = agent
        .handle(
            &client,
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{ "type": "image", "data": "xxx", "mimeType": "image/png" }]
            }),
        )
        .await;
    let err = result.expect_err("image block should be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("unsupported content type"),
        "should be a clear unsupported-type error, got: {msg}"
    );
    assert!(msg.contains("image"), "should name the offending type");
}

/// `session/prompt` 块缺 `type` 字段 → 明确「非法内容块」错误。
#[tokio::test]
async fn test_prompt_block_missing_type() {
    let agent = make_agent(Arc::new(NoopProvider));
    let client = FakeClient::new();

    let new_result = agent
        .handle(&client, "session/new", json!({}))
        .await
        .expect("session/new");
    let session_id = new_result["sessionId"].as_str().unwrap().to_string();

    let result = agent
        .handle(
            &client,
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{ "text": "no type field" }]
            }),
        )
        .await;
    let err = result.expect_err("block without type should be rejected");
    assert!(err.to_string().contains("missing or non-string 'type'"));
}

// ===== 入站消息校验（建议 1）=====

/// `classify_inbound`：`jsonrpc` 版本非法 → 错误。
#[test]
fn test_classify_inbound_bad_version() {
    let msg: InboundMessage =
        serde_json::from_str(r#"{"jsonrpc":"1.0","id":1,"method":"initialize"}"#).unwrap();
    let err = classify_inbound(&msg).expect_err("bad version should error");
    assert_eq!(err.1, -32600);
    assert!(err.2.contains("invalid jsonrpc version"));
}

/// `classify_inbound`：同时含 `method` 与 `result` → 错误。
#[test]
fn test_classify_inbound_method_and_result() {
    let msg: InboundMessage =
        serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","result":{}}"#)
            .unwrap();
    let err = classify_inbound(&msg).expect_err("method+result should error");
    assert_eq!(err.1, -32600);
    assert!(err.2.contains("both method and result/error"));
}

/// `classify_inbound`：应答缺 `result` / `error` → 错误。
#[test]
fn test_classify_inbound_response_missing_body() {
    let msg: InboundMessage = serde_json::from_str(r#"{"jsonrpc":"2.0","id":1}"#).unwrap();
    let err = classify_inbound(&msg).expect_err("response without body should error");
    assert_eq!(err.1, -32600);
    assert!(err.2.contains("missing result and error"));
}

/// `classify_inbound`：应答 id 非法（bool）→ 错误。
#[test]
fn test_classify_inbound_response_bad_id() {
    let msg: InboundMessage =
        serde_json::from_str(r#"{"jsonrpc":"2.0","id":true,"result":{}}"#).unwrap();
    let err = classify_inbound(&msg).expect_err("bad id should error");
    assert_eq!(err.1, -32600);
    assert!(err.2.contains("missing or invalid id"));
}

/// `classify_inbound`：合法请求 / notification / 应答（string id）正确分类。
#[test]
fn test_classify_inbound_valid_messages() {
    // 请求。
    let msg: InboundMessage =
        serde_json::from_str(r#"{"jsonrpc":"2.0","id":7,"method":"session/new","params":{}}"#)
            .unwrap();
    match classify_inbound(&msg).expect("valid request") {
        InboundKind::Request { id, method, .. } => {
            assert_eq!(id, Value::from(7));
            assert_eq!(method, "session/new");
        }
        _ => panic!("expected Request"),
    }

    // notification（无 id）。
    let msg: InboundMessage =
        serde_json::from_str(r#"{"jsonrpc":"2.0","method":"session/cancel"}"#).unwrap();
    assert!(matches!(
        classify_inbound(&msg).expect("valid notification"),
        InboundKind::Notification { .. }
    ));

    // 应答（string id）。
    let msg: InboundMessage =
        serde_json::from_str(r#"{"jsonrpc":"2.0","id":"abc","result":{"ok":true}}"#).unwrap();
    match classify_inbound(&msg).expect("valid response") {
        InboundKind::Response { id, result } => {
            assert_eq!(id, RequestId::String("abc".into()));
            assert!(result.is_ok());
        }
        _ => panic!("expected Response"),
    }
}
