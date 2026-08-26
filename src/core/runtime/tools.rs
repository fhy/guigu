//! 工具编排：prepare（查找 + 参数解析 + before_tool_call）→ execute
//! （顺序 / 仅 ReadOnly 组内并行 / Exclusive 独占）→ after_tool_call。
//!
//! 并发契约：
//! - `Sequential`：逐个执行。
//! - `ReadOnlyParallel`：连续的 `ReadOnly` 工具组内并行（`FuturesUnordered`，
//!   同一 task 内，共享 `on_update` 引用）；`FileWriter`/`Exclusive` 单独串行。
//! - 工具执行失败不 throw：`Err(ToolError)` / `ToolResult::is_error` 都编码进
//!   `ToolResultMessage` 进入上下文。

use std::sync::Arc;

use futures::StreamExt;
use futures::stream::FuturesUnordered;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::core::event::AgentEvent;
use crate::core::message::{ToolCall, ToolResultMessage};
use crate::core::tool::{ResourceScope, Tool, ToolResult};

use super::RunContext;

/// 一次工具调用的准备结果。
struct PreparedCall {
    tool_call: ToolCall,
    tool: Option<Arc<dyn Tool>>,
    args: serde_json::Value,
    /// `Some` 表示被否决（工具未找到 / 参数非法 / before_tool_call 否决）。
    rejected: Option<String>,
}

impl PreparedCall {
    /// 资源范围（工具未找到时按 `Exclusive` 保守处理）。
    fn scope(&self) -> ResourceScope {
        self.tool
            .as_ref()
            .map(|t| t.resource_scope())
            .unwrap_or(ResourceScope::Exclusive)
    }
}

/// prepare：查找工具 + 解析参数 + before_tool_call 钩子。
fn prepare_call(ctx: &RunContext, tc: &ToolCall) -> PreparedCall {
    let tool = ctx
        .tools
        .iter()
        .find(|t| t.name() == tc.name.as_str())
        .cloned();

    let args = match serde_json::from_str::<serde_json::Value>(&tc.arguments) {
        Ok(v) => v,
        Err(e) => {
            return PreparedCall {
                tool_call: tc.clone(),
                tool,
                args: serde_json::Value::Null,
                rejected: Some(format!("invalid arguments: {e}")),
            };
        }
    };

    let rejected = ctx
        .config
        .before_tool_call
        .as_ref()
        .and_then(|hook| hook(tc, &args).err());

    PreparedCall {
        tool_call: tc.clone(),
        tool,
        args,
        rejected,
    }
}

/// 执行单个工具调用（含 ToolExecutionStart/Update/End 事件）。
async fn execute_single(
    events_tx: &broadcast::Sender<AgentEvent>,
    prepared: &PreparedCall,
    signal: &CancellationToken,
) -> ToolResult {
    let tc = &prepared.tool_call;
    let _ = events_tx.send(AgentEvent::ToolExecutionStart {
        tool_call_id: tc.id.clone(),
        tool_name: tc.name.clone(),
        args: prepared.args.clone(),
    });

    let result = if let Some(reject) = &prepared.rejected {
        ToolResult::error(reject.clone())
    } else if let Some(tool) = &prepared.tool {
        // on_update：增量结果 → ToolExecutionUpdate 事件。
        let update_tx = events_tx.clone();
        let update_id = tc.id.clone();
        let update_name = tc.name.clone();
        let update_args = prepared.args.clone();
        let on_update: Box<dyn Fn(ToolResult) + Send + Sync> = Box::new(move |partial| {
            let _ = update_tx.send(AgentEvent::ToolExecutionUpdate {
                tool_call_id: update_id.clone(),
                tool_name: update_name.clone(),
                args: update_args.clone(),
                partial,
            });
        });
        match tool
            .execute(
                &tc.id,
                prepared.args.clone(),
                signal.clone(),
                Some(&*on_update),
            )
            .await
        {
            Ok(r) => r,
            Err(e) => ToolResult::error(e.to_string()),
        }
    } else {
        ToolResult::error(format!("unknown tool: {}", tc.name))
    };

    let _ = events_tx.send(AgentEvent::ToolExecutionEnd {
        tool_call_id: tc.id.clone(),
        tool_name: tc.name.clone(),
        result: result.clone(),
        is_error: result.is_error,
    });

    result
}

/// 顺序执行。
async fn execute_sequential(
    events_tx: &broadcast::Sender<AgentEvent>,
    prepared: &[PreparedCall],
    signal: &CancellationToken,
) -> Vec<ToolResult> {
    let mut pairs: Vec<(usize, ToolResult)> = Vec::new();
    for (i, p) in prepared.iter().enumerate() {
        let r = execute_single(events_tx, p, signal).await;
        pairs.push((i, r));
    }
    pairs.into_iter().map(|(_, r)| r).collect()
}

/// 仅 `ReadOnly` 组内并行，其余串行。
async fn execute_readonly_parallel(
    events_tx: &broadcast::Sender<AgentEvent>,
    prepared: &[PreparedCall],
    signal: &CancellationToken,
) -> Vec<ToolResult> {
    let mut pairs: Vec<(usize, ToolResult)> = Vec::new();
    let mut i = 0;
    while i < prepared.len() {
        if prepared[i].scope() == ResourceScope::ReadOnly {
            let mut j = i;
            while j < prepared.len() && prepared[j].scope() == ResourceScope::ReadOnly {
                j += 1;
            }
            let mut futures = FuturesUnordered::new();
            for k in i..j {
                let idx = k;
                futures.push(async move {
                    (idx, execute_single(events_tx, &prepared[idx], signal).await)
                });
            }
            while let Some(pair) = futures.next().await {
                pairs.push(pair);
            }
            i = j;
        } else {
            let r = execute_single(events_tx, &prepared[i], signal).await;
            pairs.push((i, r));
            i += 1;
        }
    }
    pairs.sort_by_key(|(k, _)| *k);
    pairs.into_iter().map(|(_, r)| r).collect()
}

/// 执行一批工具调用，返回按原顺序排列的 `ToolResultMessage`。
pub(crate) async fn execute_tool_calls(
    ctx: &RunContext<'_>,
    tool_calls: &[ToolCall],
    signal: &CancellationToken,
) -> Vec<ToolResultMessage> {
    let prepared: Vec<PreparedCall> = tool_calls.iter().map(|tc| prepare_call(ctx, tc)).collect();

    let results = match ctx.config.tool_execution {
        super::ToolExecutionMode::Sequential => {
            execute_sequential(ctx.events_tx, &prepared, signal).await
        }
        super::ToolExecutionMode::ReadOnlyParallel => {
            execute_readonly_parallel(ctx.events_tx, &prepared, signal).await
        }
    };

    let mut messages = Vec::new();
    for (i, result) in results.into_iter().enumerate() {
        let tc = &prepared[i].tool_call;
        let final_result = match &ctx.config.after_tool_call {
            Some(hook) => hook(tc, result),
            None => result,
        };
        messages.push(ToolResultMessage {
            tool_call_id: tc.id.clone(),
            tool_name: tc.name.clone(),
            is_error: final_result.is_error,
            content: final_result.content.clone(),
            details: final_result.details.clone(),
            timestamp: 0,
        });
    }
    messages
}
