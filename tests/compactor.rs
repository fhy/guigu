//! Task 008 集成测试：上下文摘要压缩（二期）。
//!
//! 覆盖验收分支（fake provider + fake compactor 全模拟，不依赖网络）：
//! - 超预算 → 压缩触发、摘要注入、后续请求携带压缩后 transcript
//! - 压缩失败 → 降级保守截断，agent 仍正常运行
//! - 未超预算 → 不压缩
//!
//! 同步点：以 `wait_for_idle` 为同步点。

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use common::{make_config, text_turn};
use futures::stream;
use guigu::core::compactor::{CompactionError, CompactionRequest, CompactionResult, Compactor};
use guigu::core::message::{Message, UserContent, UserMessage};
use guigu::core::provider::{
    AssistantEvent, AssistantStream, ModelProvider, ProviderError, ProviderRequest,
};
use guigu::core::{Agent, AgentHandle, AgentRuntime, CompactionPolicy, LoopConfig, Model};

// ---------- Fake compactor ----------

/// 脚本化 fake compactor：记录被压缩的消息，返回固定摘要或失败。
struct FakeCompactor {
    summary: String,
    fail: bool,
    calls: Mutex<Vec<Vec<Arc<Message>>>>,
}

impl FakeCompactor {
    fn ok(summary: &str) -> Arc<Self> {
        Arc::new(FakeCompactor {
            summary: summary.to_string(),
            fail: false,
            calls: Mutex::new(Vec::new()),
        })
    }
    fn failing() -> Arc<Self> {
        Arc::new(FakeCompactor {
            summary: String::new(),
            fail: true,
            calls: Mutex::new(Vec::new()),
        })
    }
    fn call_count(&self) -> usize {
        self.calls.lock().expect("calls mutex").len()
    }
    fn last_compacted(&self) -> Vec<Arc<Message>> {
        self.calls
            .lock()
            .expect("calls mutex")
            .last()
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait]
impl Compactor for FakeCompactor {
    async fn compact(&self, req: CompactionRequest) -> Result<CompactionResult, CompactionError> {
        self.calls.lock().expect("calls mutex").push(req.messages);
        if self.fail {
            Err(CompactionError::Provider(ProviderError::Request(
                "simulated compaction failure".to_string(),
            )))
        } else {
            Ok(CompactionResult {
                summary: self.summary.clone(),
            })
        }
    }
}

// ---------- Recording provider ----------

/// 记录型 provider：回放固定文本 turn，记录每次请求的 context.messages。
struct RecordingProvider {
    turn: Vec<AssistantEvent>,
    call_count: AtomicUsize,
    last_messages: Mutex<Vec<Message>>,
}

impl RecordingProvider {
    fn new(turn: Vec<AssistantEvent>) -> Arc<Self> {
        Arc::new(RecordingProvider {
            turn,
            call_count: AtomicUsize::new(0),
            last_messages: Mutex::new(Vec::new()),
        })
    }
    fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }
    fn last_messages(&self) -> Vec<Message> {
        self.last_messages.lock().expect("messages mutex").clone()
    }
}

#[async_trait]
impl ModelProvider for RecordingProvider {
    async fn stream(&self, request: ProviderRequest) -> Result<AssistantStream, ProviderError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        *self.last_messages.lock().expect("messages mutex") = request.context.messages.clone();
        Ok(Box::pin(stream::iter(self.turn.clone())))
    }
}

// ---------- helpers ----------

fn user_msg(text: &str) -> Message {
    Message::User(UserMessage {
        content: vec![UserContent::Text {
            text: text.to_string(),
        }],
        timestamp: 0,
    })
}

/// 构造启用 compactor 的 runtime（测试用短退避、大窗口避免一期截断干扰）。
fn make_runtime_with_compactor(
    provider: Arc<dyn ModelProvider>,
    compactor: Arc<dyn Compactor>,
    policy: CompactionPolicy,
) -> AgentRuntime {
    AgentRuntime {
        provider,
        tools: Vec::new(),
        loop_config: LoopConfig {
            model: Model {
                id: "test-model".to_string(),
                context_window: 8192,
            },
            compactor: Some(compactor),
            compaction: policy,
            retry_base_delay: Duration::from_millis(1),
            ..LoopConfig::default()
        },
    }
}

// ---------- 测试 ----------

/// 超预算 → 压缩触发、摘要注入、后续请求携带压缩后 transcript。
#[tokio::test]
async fn test_compaction_triggers_and_injects_summary() {
    let provider = RecordingProvider::new(text_turn("ok"));
    let fake = FakeCompactor::ok("SUMMARY");
    let compactor: Arc<dyn Compactor> = fake.clone();
    let policy = CompactionPolicy {
        budget_tokens: 200,
        keep_recent: 1,
    };
    let handle = AgentHandle::spawn(
        make_config(),
        make_runtime_with_compactor(provider.clone(), compactor, policy),
    );

    // 3 条大消息（每条 ~101 token，共 ~303 > 200）。
    let m0 = user_msg(&format!("m0{}", "x".repeat(400)));
    let m1 = user_msg(&format!("m1{}", "x".repeat(400)));
    let m2 = user_msg(&format!("m2{}", "x".repeat(400)));
    let m0_arc = Arc::new(m0.clone());
    let m1_arc = Arc::new(m1.clone());
    let m2_arc = Arc::new(m2.clone());
    handle
        .prompt(vec![m0.clone(), m1.clone(), m2.clone()])
        .await
        .expect("prompt should succeed");
    handle.wait_for_idle().await.expect("should settle");

    // compactor 被调用一次，压缩的是前 2 条（待压缩）。
    assert_eq!(fake.call_count(), 1, "compactor should be called once");
    assert_eq!(
        fake.last_compacted(),
        vec![m0_arc, m1_arc],
        "should compact the first (len-keep_recent) messages"
    );

    // provider 收到压缩后的 transcript：[摘要, m2]。
    assert_eq!(provider.call_count(), 1, "provider called once");
    let msgs = provider.last_messages();
    assert_eq!(msgs.len(), 2, "request should carry summary + kept message");
    assert_eq!(msgs[0], user_msg("SUMMARY"), "first should be the summary");
    assert_eq!(msgs[1], m2, "second should be the kept recent message");

    // 最终 transcript：[摘要, m2, assistant]。
    let snapshot = handle.snapshot();
    assert_eq!(snapshot.messages.len(), 3, "summary + kept + assistant");
    assert_eq!(snapshot.messages[0], Arc::new(user_msg("SUMMARY")));
    assert_eq!(snapshot.messages[1], m2_arc);
    assert!(
        matches!(snapshot.messages[2].as_ref(), Message::Assistant(_)),
        "third should be assistant"
    );
}

/// 压缩失败 → 降级保守截断（仅保留最近 keep_recent 条），agent 仍正常运行。
#[tokio::test]
async fn test_compaction_failure_degrades_to_truncation() {
    let provider = RecordingProvider::new(text_turn("ok"));
    let fake = FakeCompactor::failing();
    let compactor: Arc<dyn Compactor> = fake.clone();
    let policy = CompactionPolicy {
        budget_tokens: 200,
        keep_recent: 1,
    };
    let handle = AgentHandle::spawn(
        make_config(),
        make_runtime_with_compactor(provider.clone(), compactor, policy),
    );

    let m0 = user_msg(&format!("m0{}", "x".repeat(400)));
    let m1 = user_msg(&format!("m1{}", "x".repeat(400)));
    let m2 = user_msg(&format!("m2{}", "x".repeat(400)));
    let m2_arc = Arc::new(m2.clone());
    handle
        .prompt(vec![m0, m1, m2.clone()])
        .await
        .expect("prompt should succeed");
    handle
        .wait_for_idle()
        .await
        .expect("agent should still settle");

    // compactor 被尝试一次（失败）。
    assert_eq!(fake.call_count(), 1, "compactor should be attempted once");
    // provider 收到降级截断后的 transcript：仅 [m2]。
    let msgs = provider.last_messages();
    assert_eq!(msgs.len(), 1, "degraded to keep_recent only");
    assert_eq!(msgs[0], m2, "should keep the most recent message");

    // agent 正常产出 assistant 消息（未阻断）。
    let snapshot = handle.snapshot();
    assert_eq!(snapshot.messages.len(), 2, "kept + assistant");
    assert_eq!(snapshot.messages[0], m2_arc);
    assert!(
        matches!(snapshot.messages[1].as_ref(), Message::Assistant(_)),
        "assistant should still be produced"
    );
}

/// 未超预算 → 不压缩，transcript 原样。
#[tokio::test]
async fn test_within_budget_no_compaction() {
    let provider = RecordingProvider::new(text_turn("ok"));
    let fake = FakeCompactor::ok("SUMMARY");
    let compactor: Arc<dyn Compactor> = fake.clone();
    let policy = CompactionPolicy {
        budget_tokens: 10_000,
        keep_recent: 1,
    };
    let handle = AgentHandle::spawn(
        make_config(),
        make_runtime_with_compactor(provider.clone(), compactor, policy),
    );

    let m0 = user_msg("hello");
    let m1 = user_msg("world");
    handle
        .prompt(vec![m0.clone(), m1.clone()])
        .await
        .expect("prompt should succeed");
    handle.wait_for_idle().await.expect("should settle");

    assert_eq!(fake.call_count(), 0, "within budget should not compact");
    let msgs = provider.last_messages();
    assert_eq!(msgs.len(), 2, "both messages should be sent");
    assert_eq!(msgs[0], m0);
    assert_eq!(msgs[1], m1);
}

/// 持久化语义：压缩结果回写 transcript，后续 turn 不重复压缩、不恢复旧消息
/// （验证「非单次请求投影」的持久契约，避免调用方误以为只是单次请求投影）。
#[tokio::test]
async fn test_compaction_persistent_no_recompact() {
    let provider = RecordingProvider::new(text_turn("ok"));
    let fake = FakeCompactor::ok("SUMMARY");
    let compactor: Arc<dyn Compactor> = fake.clone();
    let policy = CompactionPolicy {
        budget_tokens: 200,
        keep_recent: 1,
    };
    let handle = AgentHandle::spawn(
        make_config(),
        make_runtime_with_compactor(provider.clone(), compactor, policy),
    );

    // 第一次 prompt：3 条大消息（超预算）→ 压缩触发。
    let m0 = user_msg(&format!("m0{}", "x".repeat(400)));
    let m1 = user_msg(&format!("m1{}", "x".repeat(400)));
    let m2 = user_msg(&format!("m2{}", "x".repeat(400)));
    handle
        .prompt(vec![m0, m1, m2.clone()])
        .await
        .expect("first prompt should succeed");
    handle.wait_for_idle().await.expect("should settle");

    // 压缩触发一次。
    assert_eq!(fake.call_count(), 1, "compactor should be called once");

    // 第二次 prompt：小消息（transcript 现已预算内）→ 不重复压缩。
    handle
        .prompt(vec![user_msg("hi")])
        .await
        .expect("second prompt should succeed");
    handle.wait_for_idle().await.expect("should settle");

    // compactor 仍只被调用一次（未重复压缩）。
    assert_eq!(
        fake.call_count(),
        1,
        "should not re-compact on subsequent turn"
    );

    // transcript 仍含摘要（未恢复旧消息）。
    let snapshot = handle.snapshot();
    let has_summary = snapshot.messages.iter().any(|m| {
        matches!(m.as_ref(), Message::User(u) if u.content.first()
            == Some(&UserContent::Text { text: "SUMMARY".to_string() }))
    });
    assert!(has_summary, "summary should still be in transcript");
    // 旧消息 m0 不应恢复。
    let m0_restored = snapshot.messages.iter().any(|m| {
        m.as_ref()
            == &Message::User(UserMessage {
                content: vec![UserContent::Text {
                    text: format!("m0{}", "x".repeat(400)),
                }],
                timestamp: 0,
            })
    });
    assert!(!m0_restored, "m0 should not be restored");
}
