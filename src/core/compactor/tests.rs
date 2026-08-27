//! `compactor` 模块单元测试（fake provider 驱动，不依赖网络）。

use super::*;
use crate::core::message::{
    AssistantMessage, StopReason, ToolCall, ToolResultMessage, UserMessage,
};
use futures::stream;
use std::sync::Mutex;

/// 脚本化 fake provider：回放固定事件序列，记录收到的请求。
struct ScriptedProvider {
    events: Vec<AssistantEvent>,
    fail: bool,
    last_request: Mutex<Option<ProviderRequest>>,
}

impl ScriptedProvider {
    fn new(events: Vec<AssistantEvent>) -> Arc<Self> {
        Arc::new(ScriptedProvider {
            events,
            fail: false,
            last_request: Mutex::new(None),
        })
    }
    fn failing() -> Arc<Self> {
        Arc::new(ScriptedProvider {
            events: Vec::new(),
            fail: true,
            last_request: Mutex::new(None),
        })
    }
    fn last_request(&self) -> Option<ProviderRequest> {
        self.last_request.lock().expect("request mutex").clone()
    }
}

/// 永不产事件的 provider（用于取消路径：让 `stream.next()` 挂起）。
struct PendingProvider;

#[async_trait]
impl ModelProvider for PendingProvider {
    async fn stream(
        &self,
        _request: ProviderRequest,
    ) -> Result<crate::core::provider::AssistantStream, ProviderError> {
        Ok(Box::pin(stream::pending::<AssistantEvent>()))
    }
}

#[async_trait]
impl ModelProvider for ScriptedProvider {
    async fn stream(
        &self,
        request: ProviderRequest,
    ) -> Result<crate::core::provider::AssistantStream, ProviderError> {
        *self.last_request.lock().expect("request mutex") = Some(request);
        if self.fail {
            return Err(ProviderError::Request(
                "simulated provider failure".to_string(),
            ));
        }
        Ok(Box::pin(stream::iter(self.events.clone())))
    }
}

fn model() -> Model {
    Model {
        id: "summary-model".to_string(),
        context_window: 4096,
    }
}

fn user_msg(text: &str) -> Arc<Message> {
    Arc::new(Message::User(UserMessage {
        content: vec![UserContent::Text {
            text: text.to_string(),
        }],
        timestamp: 0,
    }))
}

fn assistant_msg(text: &str) -> Arc<Message> {
    Arc::new(Message::Assistant(AssistantMessage {
        content: vec![AssistantContent::Text {
            text: text.to_string(),
        }],
        model: None,
        usage: None,
        stop_reason: Some(StopReason::Completed),
        error_message: None,
        timestamp: 0,
    }))
}

fn done_event(text: &str) -> AssistantEvent {
    AssistantEvent::Done {
        message: AssistantMessage {
            content: vec![AssistantContent::Text {
                text: text.to_string(),
            }],
            model: None,
            usage: None,
            stop_reason: Some(StopReason::Completed),
            error_message: None,
            timestamp: 0,
        },
    }
}

/// 摘要文本正确累积；请求中 system_prompt / messages / tools 符合契约。
#[tokio::test]
async fn test_compact_accumulates_text_and_builds_request() {
    let provider = ScriptedProvider::new(vec![
        AssistantEvent::TextDelta {
            text: "Hello".to_string(),
        },
        AssistantEvent::TextDelta {
            text: " world".to_string(),
        },
        done_event("Hello world"),
    ]);
    let compactor = LlmCompactor::new(provider.clone(), model(), "summarize this");
    let messages = vec![user_msg("a"), assistant_msg("b")];
    let result = compactor
        .compact(CompactionRequest {
            messages: messages.clone(),
            signal: CancellationToken::new(),
        })
        .await
        .expect("compact should succeed");
    assert_eq!(
        result.summary, "Hello world",
        "summary should accumulate TextDelta"
    );

    let req = provider.last_request().expect("provider should be called");
    assert_eq!(req.context.system_prompt, "summarize this");
    assert_eq!(
        req.context.messages.len(),
        2,
        "messages should be the input messages"
    );
    assert_eq!(req.context.messages[0], *messages[0]);
    assert_eq!(req.context.messages[1], *messages[1]);
    assert!(req.context.tools.is_empty(), "tools should be empty");
}

/// provider 外层 Err → CompactionError::Provider。
#[tokio::test]
async fn test_compact_provider_outer_error() {
    let compactor = LlmCompactor::new(ScriptedProvider::failing(), model(), "p");
    let result = compactor
        .compact(CompactionRequest {
            messages: vec![user_msg("a")],
            signal: CancellationToken::new(),
        })
        .await;
    assert!(
        matches!(result, Err(CompactionError::Provider(_))),
        "outer provider error should map to Provider"
    );
}

/// 流内 Error → CompactionError::Provider。
#[tokio::test]
async fn test_compact_stream_inner_error() {
    let provider = ScriptedProvider::new(vec![AssistantEvent::Error {
        message: "boom".to_string(),
        aborted: false,
    }]);
    let compactor = LlmCompactor::new(provider, model(), "p");
    let result = compactor
        .compact(CompactionRequest {
            messages: vec![user_msg("a")],
            signal: CancellationToken::new(),
        })
        .await;
    assert!(
        matches!(result, Err(CompactionError::Provider(_))),
        "in-stream error should map to Provider"
    );
}

/// 空输入 → EmptyInput。
#[tokio::test]
async fn test_compact_empty_input() {
    let compactor = LlmCompactor::new(ScriptedProvider::new(vec![]), model(), "p");
    let result = compactor
        .compact(CompactionRequest {
            messages: Vec::new(),
            signal: CancellationToken::new(),
        })
        .await;
    assert!(matches!(result, Err(CompactionError::EmptyInput)));
}

/// 调用前已取消 → Cancelled。
#[tokio::test]
async fn test_compact_pre_cancelled() {
    let compactor = LlmCompactor::new(ScriptedProvider::new(vec![]), model(), "p");
    let signal = CancellationToken::new();
    signal.cancel();
    let result = compactor
        .compact(CompactionRequest {
            messages: vec![user_msg("a")],
            signal,
        })
        .await;
    assert!(matches!(result, Err(CompactionError::Cancelled)));
}

/// 累积期间取消（流挂起）→ Cancelled。
#[tokio::test]
async fn test_compact_cancelled_during_accumulation() {
    let compactor = LlmCompactor::new(Arc::new(PendingProvider), model(), "p");
    let signal = CancellationToken::new();
    let req = CompactionRequest {
        messages: vec![user_msg("a")],
        signal: signal.clone(),
    };
    let handle = tokio::spawn(async move { compactor.compact(req).await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    signal.cancel();
    let result = handle.await.expect("task should not panic");
    assert!(matches!(result, Err(CompactionError::Cancelled)));
}

/// 默认拼接格式：User/Assistant/ToolResult 混合序列，稳定可断言。
#[test]
fn test_format_messages_default() {
    let tool_result = Arc::new(Message::ToolResult(ToolResultMessage {
        tool_call_id: "c1".to_string(),
        tool_name: "echo".to_string(),
        is_error: false,
        content: vec![ToolResultContent::Text {
            text: "result".to_string(),
        }],
        details: None,
        timestamp: 0,
    }));
    let assistant = Arc::new(Message::Assistant(AssistantMessage {
        content: vec![
            AssistantContent::Text {
                text: "hi".to_string(),
            },
            AssistantContent::ToolCall(ToolCall {
                id: "c1".to_string(),
                name: "echo".to_string(),
                arguments: "{\"x\":1}".to_string(),
            }),
        ],
        model: None,
        usage: None,
        stop_reason: None,
        error_message: None,
        timestamp: 0,
    }));
    let messages = vec![user_msg("hello"), assistant, tool_result];
    let formatted = format_messages_for_summary(&messages);
    assert_eq!(
        formatted, "[user] hello\n[assistant] hi\ntool_call:echo\n[tool_result:echo] result",
        "default format should be stable: [role] content per line"
    );
}

/// 默认拼接格式：多段 content 用 \n 拼接，Thinking 忽略。
#[test]
fn test_format_messages_multisegment_and_thinking_ignored() {
    let user = Arc::new(Message::User(UserMessage {
        content: vec![
            UserContent::Text {
                text: "line1".to_string(),
            },
            UserContent::Text {
                text: "line2".to_string(),
            },
        ],
        timestamp: 0,
    }));
    let assistant = Arc::new(Message::Assistant(AssistantMessage {
        content: vec![
            AssistantContent::Thinking {
                text: "hidden".to_string(),
            },
            AssistantContent::Text {
                text: "visible".to_string(),
            },
        ],
        model: None,
        usage: None,
        stop_reason: None,
        error_message: None,
        timestamp: 0,
    }));
    let formatted = format_messages_for_summary(&[user, assistant]);
    assert_eq!(formatted, "[user] line1\nline2\n[assistant] visible");
}
