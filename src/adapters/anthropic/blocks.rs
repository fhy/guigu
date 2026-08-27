//! Anthropic `content_block_*` 事件处理（纯函数）。
//!
//! `content_block_start` / `content_block_delta` / `content_block_stop`
//! 由 [`super::events::dispatch`] 解析 JSON 后分发至此。
//!
//! 每个 text/thinking block 按 content_block index **独立累积**（多个同类型
//! block 不合并）；缺少/非法 `index` 或 block 类型字段返回可诊断的
//! `ProviderError::Parse`，避免异常响应污染第 0 个 block。

use serde_json::Value;

use crate::core::provider::{AssistantEvent, ProviderError};

use super::super::acc::{Acc, SegmentKind};

/// 提取并校验 `index` 字段（缺失/非数字 → `Parse`）。
fn block_index(v: &Value, event: &str) -> Result<usize, ProviderError> {
    v.get("index")
        .and_then(|i| i.as_u64())
        .map(|i| i as usize)
        .ok_or_else(|| ProviderError::Parse(format!("{event} missing valid index")))
}

/// `content_block_start`：tool_use → `ToolCallStart`；text/thinking 新建独立 block。
pub(crate) fn handle_block_start(
    v: &Value,
    acc: &mut Acc,
) -> Result<Vec<AssistantEvent>, ProviderError> {
    let index = block_index(v, "content_block_start")?;
    // 同一 index 重复 start 是协议违规：必须在创建/登记新 block 之前拒绝，
    // 否则新段先入 `segments`、`note_block` 再覆盖旧映射，留下无法定位的幽灵段。
    if acc.block_kind(index).is_some() {
        return Err(ProviderError::Parse(format!(
            "duplicate content_block_start at index {index}"
        )));
    }
    let block = v
        .get("content_block")
        .ok_or_else(|| ProviderError::Parse("content_block_start missing content_block".into()))?;
    let block_type = block.get("type").and_then(|t| t.as_str()).ok_or_else(|| {
        ProviderError::Parse("content_block_start missing content_block.type".into())
    })?;
    match block_type {
        "tool_use" => {
            let id = block.get("id").and_then(|i| i.as_str()).ok_or_else(|| {
                ProviderError::Parse("content_block_start tool_use missing id".into())
            })?;
            let name = block.get("name").and_then(|n| n.as_str()).ok_or_else(|| {
                ProviderError::Parse("content_block_start tool_use missing name".into())
            })?;
            let tc_idx = acc.start_tool_call(id.to_string(), name.to_string());
            acc.note_block(index, SegmentKind::ToolCall(tc_idx));
            Ok(vec![AssistantEvent::ToolCallStart {
                id: id.to_string(),
                name: name.to_string(),
                arguments: String::new(),
            }])
        }
        "text" => {
            let seg = acc.start_text_block();
            acc.note_block(index, SegmentKind::Text(seg));
            Ok(Vec::new())
        }
        "thinking" => {
            let seg = acc.start_thinking_block();
            acc.note_block(index, SegmentKind::Thinking(seg));
            Ok(Vec::new())
        }
        other => Err(ProviderError::Parse(format!(
            "content_block_start unknown block type: {other}"
        ))),
    }
}

/// `content_block_delta`：text_delta / thinking_delta / input_json_delta。
pub(crate) fn handle_block_delta(
    v: &Value,
    acc: &mut Acc,
) -> Result<Vec<AssistantEvent>, ProviderError> {
    let index = block_index(v, "content_block_delta")?;
    let delta = v
        .get("delta")
        .ok_or_else(|| ProviderError::Parse("content_block_delta missing delta".into()))?;
    let delta_type = delta
        .get("type")
        .and_then(|t| t.as_str())
        .ok_or_else(|| ProviderError::Parse("content_block_delta missing delta.type".into()))?;
    match delta_type {
        "text_delta" => {
            let text = delta
                .get("text")
                .and_then(|t| t.as_str())
                .ok_or_else(|| ProviderError::Parse("text_delta missing text".into()))?;
            let seg = match acc.block_kind(index) {
                Some(SegmentKind::Text(i)) => *i,
                other => {
                    return Err(ProviderError::Parse(format!(
                        "text_delta for block {index} without text block start (kind: {other:?})"
                    )));
                }
            };
            acc.append_text_block(seg, text);
            Ok(vec![AssistantEvent::TextDelta {
                text: text.to_string(),
            }])
        }
        "thinking_delta" => {
            let thinking = delta
                .get("thinking")
                .and_then(|t| t.as_str())
                .ok_or_else(|| ProviderError::Parse("thinking_delta missing thinking".into()))?;
            let seg = match acc.block_kind(index) {
                Some(SegmentKind::Thinking(i)) => *i,
                other => {
                    return Err(ProviderError::Parse(format!(
                        "thinking_delta for block {index} without thinking block start (kind: {other:?})"
                    )));
                }
            };
            acc.append_thinking_block(seg, thinking);
            Ok(vec![AssistantEvent::ThinkingDelta {
                thinking: thinking.to_string(),
            }])
        }
        "input_json_delta" => {
            let partial = delta
                .get("partial_json")
                .and_then(|t| t.as_str())
                .ok_or_else(|| {
                    ProviderError::Parse("input_json_delta missing partial_json".into())
                })?;
            let tc_idx = match acc.block_kind(index) {
                Some(SegmentKind::ToolCall(i)) => *i,
                other => {
                    return Err(ProviderError::Parse(format!(
                        "input_json_delta for block {index} without tool_use block start (kind: {other:?})"
                    )));
                }
            };
            let id = acc
                .tool_calls
                .get(tc_idx)
                .map(|t| t.id.clone())
                .ok_or_else(|| {
                    ProviderError::Parse(format!("tool call {tc_idx} missing for block {index}"))
                })?;
            acc.tool_calls[tc_idx].arguments.push_str(partial);
            Ok(vec![AssistantEvent::ToolCallDelta {
                id,
                arguments_delta: partial.to_string(),
            }])
        }
        other => Err(ProviderError::Parse(format!(
            "content_block_delta unknown delta type: {other}"
        ))),
    }
}

/// `content_block_stop`：tool_use → `ToolCallEnd`；text/thinking 不产出事件。
pub(crate) fn handle_block_stop(
    v: &Value,
    acc: &mut Acc,
) -> Result<Vec<AssistantEvent>, ProviderError> {
    let index = block_index(v, "content_block_stop")?;
    match acc.block_kind(index) {
        Some(SegmentKind::ToolCall(tc_idx)) => {
            // 先取出 (id, done) 再释放借用，避免与后续 `end_tool_call` 的可变借用冲突。
            let (id, done) = {
                let tc = acc.tool_calls.get(*tc_idx).ok_or_else(|| {
                    ProviderError::Parse(format!("tool call {tc_idx} missing for block {index}"))
                })?;
                (tc.id.clone(), tc.done)
            };
            // 同一 tool call 重复 stop 是协议违规：拒绝，避免重复发 `ToolCallEnd`。
            if done {
                return Err(ProviderError::Parse(format!(
                    "duplicate content_block_stop for tool call {id} at block index {index}"
                )));
            }
            acc.end_tool_call(&id);
            Ok(vec![AssistantEvent::ToolCallEnd { id }])
        }
        Some(SegmentKind::Text(_)) | Some(SegmentKind::Thinking(_)) => Ok(Vec::new()),
        None => Err(ProviderError::Parse(format!(
            "content_block_stop for unknown block index {index}"
        ))),
    }
}

#[cfg(test)]
mod tests;
