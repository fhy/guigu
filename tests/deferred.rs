//! Task 011 集成测试：DeferredTool 走完整 Tool trait 契约 + 并发只构建一次。
//!
//! 以 `Arc<dyn Tool>` 验证：schema 方法不触发工厂、execute 透传 tool_call_id/args
//! 并返回内层结果、多 task 并发 execute 下工厂仅调用一次（OnceLock 保证）。
//! fake tool 用内存记录，不依赖外部服务或硬编码路径。

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use guigu::core::message::ToolResultContent;
use guigu::core::tool::{ResourceScope, Tool, ToolError, ToolResult};
use guigu::{DeferredTool, DeferredToolSpec, EchoTool};
use tokio_util::sync::CancellationToken;

/// 记录型 fake tool：记录收到的 tool_call_id/args，返回确定性结果。
#[derive(Debug)]
struct RecordingTool {
    received: Arc<Mutex<(String, serde_json::Value)>>,
}

#[async_trait]
impl Tool for RecordingTool {
    fn name(&self) -> &str {
        "rec"
    }
    fn description(&self) -> &str {
        "recording fake tool"
    }
    fn parameters(&self) -> Option<serde_json::Value> {
        None
    }
    fn resource_scope(&self) -> ResourceScope {
        ResourceScope::ReadOnly
    }
    async fn execute(
        &self,
        tool_call_id: &str,
        args: serde_json::Value,
        _signal: CancellationToken,
        _on_update: Option<&(dyn Fn(ToolResult) + Send + Sync)>,
    ) -> Result<ToolResult, ToolError> {
        *self.received.lock().unwrap() = (tool_call_id.to_string(), args);
        Ok(ToolResult::text(format!("rec:{tool_call_id}")))
    }
}

/// 完整 Tool trait 契约：schema 方法不触发工厂，execute 透传并返回内层结果。
#[tokio::test]
async fn test_deferred_full_contract() {
    let counter = Arc::new(AtomicUsize::new(0));
    let c = counter.clone();
    let received = Arc::new(Mutex::new((String::new(), serde_json::Value::Null)));
    let rec = received.clone();

    let tool: Arc<dyn Tool> = Arc::new(DeferredTool::lazy(
        DeferredToolSpec {
            name: "rec".into(),
            description: "recording".into(),
            parameters: Some(serde_json::json!({ "type": "object" })),
            resource_scope: ResourceScope::ReadOnly,
        },
        move || {
            c.fetch_add(1, Ordering::SeqCst);
            Arc::new(RecordingTool {
                received: rec.clone(),
            })
        },
    ));

    // schema 方法：不触发工厂
    assert_eq!(tool.name(), "rec");
    assert_eq!(tool.description(), "recording");
    assert_eq!(
        tool.parameters(),
        Some(serde_json::json!({ "type": "object" }))
    );
    assert_eq!(tool.resource_scope(), ResourceScope::ReadOnly);
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "schema methods must not build"
    );

    // execute：透传 tool_call_id/args，返回内层结果
    let args = serde_json::json!({ "a": 1 });
    let result = tool
        .execute("call-7", args.clone(), CancellationToken::new(), None)
        .await
        .expect("execute should succeed");
    assert_eq!(counter.load(Ordering::SeqCst), 1, "factory runs once");
    let (id, got_args) = received.lock().unwrap().clone();
    assert_eq!(id, "call-7");
    assert_eq!(got_args, args);
    assert_eq!(
        result.content[0],
        ToolResultContent::Text {
            text: "rec:call-7".to_string()
        }
    );
    assert!(!result.is_error);
}

/// 并发只构建一次：多 task 并发 execute，工厂计数仍为 1。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_concurrent_execute_builds_once() {
    let counter = Arc::new(AtomicUsize::new(0));
    let c = counter.clone();
    let tool: Arc<dyn Tool> = Arc::new(DeferredTool::lazy(
        DeferredToolSpec {
            name: "echo".into(),
            description: "echo".into(),
            parameters: None,
            resource_scope: ResourceScope::ReadOnly,
        },
        move || {
            c.fetch_add(1, Ordering::SeqCst);
            Arc::new(EchoTool)
        },
    ));

    let mut handles = Vec::new();
    for i in 0..32 {
        let tool = Arc::clone(&tool);
        handles.push(tokio::spawn(async move {
            tool.execute(
                &format!("call-{i}"),
                serde_json::json!({ "message": format!("m{i}") }),
                CancellationToken::new(),
                None,
            )
            .await
        }));
    }
    for r in futures::future::join_all(handles).await {
        r.expect("task should not panic")
            .expect("execute should succeed");
    }
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "factory must run exactly once under concurrency"
    );
}
