//! SSE 集成测试 helper（Task 025）：`SseClient`（reqwest SSE 客户端）+ server 启动
//! + 内存 storage / agent 构造。
//!
//! 从 `mod.rs` 拆出（单文件 ≤ 400 行约束）。

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use futures::{Stream, StreamExt};
use guigu::acp::AcpAgent;
use guigu::core::agent::AgentConfig;
use guigu::core::message::ThinkingLevel;
use guigu::core::provider::Model;
use guigu::core::runtime::{AgentRuntime, LoopConfig};
use guigu::core::session::{
    NodeId, SessionEntry, SessionError, SessionStorage, SessionTree, reduce,
};
use guigu::server::AgentServer;
use serde_json::{Value, json};

use super::common::{FakeProvider, text_turn};

/// 内存 `SessionStorage`（测试用，同 `tests/acp.rs`）。
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

#[async_trait::async_trait]
impl SessionStorage for InMemoryStorage {
    async fn append(
        &self,
        parent_id: Option<NodeId>,
        message: guigu::core::message::Message,
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

/// 建一个配置好 runtime / storage 工厂的 `AcpAgent`（单文本 turn provider）。
///
/// 每个 lane 创建**独立** `FakeProvider`（`call_index` 不跨 lane 递增），使多
/// session / 多 client 并发时各自 prompt 都能拿到完整 turn（否则共享 provider 的
/// 第二个 prompt 会拿到空 turn，无 `session/update`）。
pub fn make_agent() -> AcpAgent {
    let server = AgentServer::new();
    server.with_runtime_factory(|| {
        let provider: Arc<dyn guigu::core::provider::ModelProvider> =
            FakeProvider::new(vec![text_turn("ok")]);
        (
            AgentConfig {
                system_prompt: "test".to_string(),
                model: Some("test-model".to_string()),
                thinking_level: ThinkingLevel::Off,
            },
            AgentRuntime {
                provider,
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

/// 取一个空闲端口（bind :0 → 取端口 → drop；测试用，竞态窗口极小）。
async fn free_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    port
}

/// 起 server 并等待就绪（轮询 TCP 连接）。返回 server task（测试结束 abort）。
pub async fn start_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let port = free_port().await;
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let agent = make_agent();
    let task = tokio::spawn(async move {
        let _ = agent.serve_sse(addr).await;
    });
    // 等待 server 就绪。
    for _ in 0..200 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return (addr, task);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("server not ready at {addr}");
}

/// SSE 客户端：连接 `GET /sse`、读事件流、`POST /message`。
pub struct SseClient {
    addr: SocketAddr,
    client_id: String,
    http: reqwest::Client,
    stream: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    buf: String,
}

impl SseClient {
    /// 连接 SSE 并读首个握手 event（`client_id`）。
    pub async fn connect(addr: SocketAddr) -> Self {
        let http = reqwest::Client::new();
        let response = http
            .get(format!("http://{addr}/sse"))
            .header("Accept", "text/event-stream")
            .send()
            .await
            .expect("sse connect");
        let stream = response.bytes_stream();
        let mut client = Self {
            addr,
            client_id: String::new(),
            http,
            stream: Box::pin(stream),
            buf: String::new(),
        };
        let (event, data) = client
            .next_event()
            .await
            .expect("should receive handshake event");
        assert_eq!(event, "client_id", "first event should be client_id");
        let parsed: Value = serde_json::from_str(&data).expect("client_id json");
        client.client_id = parsed["client_id"].as_str().expect("client_id").to_string();
        client
    }

    /// 读下一个 SSE event（`(event, data)`）；流结束返回 `None`。
    pub async fn next_event(&mut self) -> Option<(String, String)> {
        loop {
            if let Some(parsed) = self.try_parse() {
                return Some(parsed);
            }
            match self.stream.next().await {
                Some(Ok(chunk)) => self.buf.push_str(&String::from_utf8_lossy(&chunk)),
                _ => return None,
            }
        }
    }

    /// 从缓冲解析一个完整 SSE frame（以 `\n\n` 结尾）；未完成返回 `None`。
    fn try_parse(&mut self) -> Option<(String, String)> {
        let frame_end = self.buf.find("\n\n")?;
        let frame = self.buf[..frame_end].to_string();
        self.buf.drain(..frame_end + 2);
        let mut event = String::new();
        let mut data = String::new();
        for line in frame.lines() {
            if let Some(value) = line.strip_prefix("event:") {
                event = value.trim().to_string();
            } else if let Some(value) = line.strip_prefix("data:") {
                data.push_str(value.trim());
            }
        }
        Some((event, data))
    }

    /// 本连接的 `client_id`（测试用）。
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// `POST /message`（带 `X-Client-Id`），返回 HTTP 状态码。
    pub async fn post(&self, msg: Value) -> u16 {
        let resp = self
            .http
            .post(format!("http://{}/message", self.addr))
            .header("X-Client-Id", &self.client_id)
            .json(&msg)
            .send()
            .await
            .expect("post");
        let status = resp.status().as_u16();
        let _ = resp.bytes().await;
        status
    }

    /// `POST /message`（**不带** `X-Client-Id`，测试缺 header 路径），返回状态码。
    pub async fn post_no_client_id(&self, msg: Value) -> u16 {
        let resp = self
            .http
            .post(format!("http://{}/message", self.addr))
            .json(&msg)
            .send()
            .await
            .expect("post");
        let status = resp.status().as_u16();
        let _ = resp.bytes().await;
        status
    }

    /// `POST` 一条请求，再读事件直到收到 `id` 对应的 response，返回该 response。
    pub async fn wait_response_after(&mut self, id: u64, timeout: Duration, msg: Value) -> Value {
        let status = self.post(msg).await;
        assert_eq!(status, 202, "request should be accepted");
        self.wait_response(id, timeout).await
    }

    /// 读事件直到收到 `id` 对应的 response（跳过 notification），返回该 response。
    pub async fn wait_response(&mut self, id: u64, timeout: Duration) -> Value {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                panic!("timeout waiting for response id={id}");
            }
            let (event, data) = tokio::time::timeout(remaining, self.next_event())
                .await
                .expect("timeout")
                .expect("stream ended");
            let msg: Value = serde_json::from_str(&data).expect("valid json");
            if event == "response" && msg["id"].as_u64() == Some(id) {
                return msg;
            }
        }
    }

    /// `POST session/prompt`，读事件直到 response 到达；返回 (沿途 session/update, response)。
    pub async fn prompt(
        &mut self,
        session_id: &str,
        id: u64,
        timeout: Duration,
    ) -> (Vec<Value>, Value) {
        let status = self
            .post(json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "session/prompt",
                "params": {
                    "sessionId": session_id,
                    "prompt": [{ "type": "text", "text": "hi" }]
                }
            }))
            .await;
        assert_eq!(status, 202, "prompt should be accepted");
        let mut updates = Vec::new();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                panic!("timeout waiting for prompt response id={id}");
            }
            let (event, data) = tokio::time::timeout(remaining, self.next_event())
                .await
                .expect("timeout")
                .expect("stream ended");
            let msg: Value = serde_json::from_str(&data).expect("valid json");
            if event == "response" && msg["id"].as_u64() == Some(id) {
                return (updates, msg);
            }
            if event == "session/update" {
                updates.push(msg);
            }
        }
    }
}
