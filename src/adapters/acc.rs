//! 流累积状态（OpenAI / Anthropic 适配器共享）。
//!
//! 累积规则：
//! 1. `TextDelta`/`ThinkingDelta` 按到达顺序追加到 `text`/`thinking`，
//!    并记录其相对 `tool_calls` 的顺序（用于最终 content 排序）。
//! 2. `ToolCallStart` 追加新 `ToolCallAcc`；`ToolCallDelta` 按 id 累积 arguments；
//!    `ToolCallEnd` 标记该项完成。
//! 3. 流结束 → [`Acc::build_message`] 按 segment 顺序构造完整 `AssistantMessage`。

use crate::core::message::{
    AssistantContent, AssistantMessage, ModelId, StopReason, ToolCall, Usage,
};

/// 内容段种类（决定最终 content 顺序）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SegmentKind {
    /// 文本段（映射到 [`Acc::text`]）。
    Text,
    /// 思考段（映射到 [`Acc::thinking`]）。
    Thinking,
    /// 工具调用段（索引指向 [`Acc::tool_calls`]）。
    ToolCall(usize),
}

/// 单个工具调用的累积状态。
#[derive(Debug, Clone)]
pub(crate) struct ToolCallAcc {
    pub id: String,
    pub name: String,
    /// 流式累积的 arguments JSON 字符串（结束前可为部分）。
    pub arguments: String,
    /// 是否已发 `ToolCallEnd`。
    pub done: bool,
}

/// 流累积状态。
#[derive(Debug)]
pub(crate) struct Acc {
    /// 累积文本。
    pub text: String,
    /// 累积思考。
    pub thinking: String,
    /// 工具调用累积（按 start 顺序）。
    pub tool_calls: Vec<ToolCallAcc>,
    /// 内容段顺序（首次出现 / block index 顺序）。
    pub segments: Vec<SegmentKind>,
    /// Anthropic content_block index → 段种类（用于 stop 时识别 tool_use）。
    pub block_kinds: Vec<Option<SegmentKind>>,
    /// 用量（末 chunk 填入）。
    pub usage: Option<Usage>,
    /// 停止原因（finish 时填入）。
    pub stop_reason: Option<StopReason>,
    /// 透传的模型标识。
    pub model: String,
    /// Anthropic `message_start` 提供的 input_tokens（与 `message_delta` 的 output 合并）。
    pub input_tokens: u64,
}

impl Acc {
    /// 新建累积状态。
    pub fn new(model: String) -> Self {
        Self {
            text: String::new(),
            thinking: String::new(),
            tool_calls: Vec::new(),
            segments: Vec::new(),
            block_kinds: Vec::new(),
            usage: None,
            stop_reason: None,
            model,
            input_tokens: 0,
        }
    }

    /// 确保存在 Text 段（首次出现时创建，保持顺序）。
    pub fn ensure_text(&mut self) {
        if !self.segments.iter().any(|s| matches!(s, SegmentKind::Text)) {
            self.segments.push(SegmentKind::Text);
        }
    }

    /// 确保存在 Thinking 段（首次出现时创建，保持顺序）。
    pub fn ensure_thinking(&mut self) {
        if !self
            .segments
            .iter()
            .any(|s| matches!(s, SegmentKind::Thinking))
        {
            self.segments.push(SegmentKind::Thinking);
        }
    }

    /// 追加文本增量。
    pub fn append_text(&mut self, text: &str) {
        self.ensure_text();
        self.text.push_str(text);
    }

    /// 追加思考增量。
    pub fn append_thinking(&mut self, thinking: &str) {
        self.ensure_thinking();
        self.thinking.push_str(thinking);
    }

    /// 追加新工具调用，返回其在 `tool_calls` 中的索引。
    pub fn start_tool_call(&mut self, id: String, name: String) -> usize {
        let idx = self.tool_calls.len();
        self.tool_calls.push(ToolCallAcc {
            id,
            name,
            arguments: String::new(),
            done: false,
        });
        self.segments.push(SegmentKind::ToolCall(idx));
        idx
    }

    /// 按 id 标记工具调用完成。
    pub fn end_tool_call(&mut self, id: &str) {
        if let Some(tc) = self.tool_calls.iter_mut().find(|tc| tc.id == id) {
            tc.done = true;
        }
    }

    /// 记录 Anthropic content_block index 对应的段种类。
    pub fn note_block(&mut self, index: usize, kind: SegmentKind) {
        if self.block_kinds.len() <= index {
            self.block_kinds.resize(index + 1, None);
        }
        self.block_kinds[index] = Some(kind);
    }

    /// 查询 Anthropic content_block index 对应的段种类。
    pub fn block_kind(&self, index: usize) -> Option<&SegmentKind> {
        self.block_kinds.get(index).and_then(|k| k.as_ref())
    }

    /// 按 segment 顺序构造完整 `AssistantMessage`。
    pub fn build_message(&self) -> AssistantMessage {
        let mut content = Vec::new();
        for seg in &self.segments {
            match seg {
                SegmentKind::Text => {
                    if !self.text.is_empty() {
                        content.push(AssistantContent::Text {
                            text: self.text.clone(),
                        });
                    }
                }
                SegmentKind::Thinking => {
                    if !self.thinking.is_empty() {
                        content.push(AssistantContent::Thinking {
                            text: self.thinking.clone(),
                        });
                    }
                }
                SegmentKind::ToolCall(i) => {
                    if let Some(tc) = self.tool_calls.get(*i) {
                        content.push(AssistantContent::ToolCall(ToolCall {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            arguments: tc.arguments.clone(),
                        }));
                    }
                }
            }
        }
        AssistantMessage {
            content,
            model: Some(ModelId(self.model.clone())),
            usage: self.usage.clone(),
            stop_reason: self.stop_reason.clone(),
            error_message: None,
            timestamp: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::message::AssistantContent;

    #[test]
    fn text_accumulation() {
        let mut acc = Acc::new("m".into());
        acc.append_text("Hello, ");
        acc.append_text("world");
        assert_eq!(acc.text, "Hello, world");
        let msg = acc.build_message();
        assert_eq!(
            msg.content,
            vec![AssistantContent::Text {
                text: "Hello, world".into()
            }]
        );
    }

    #[test]
    fn thinking_before_text_order() {
        let mut acc = Acc::new("m".into());
        acc.append_thinking("hmm");
        acc.append_text("answer");
        let msg = acc.build_message();
        assert_eq!(
            msg.content,
            vec![
                AssistantContent::Thinking { text: "hmm".into() },
                AssistantContent::Text {
                    text: "answer".into()
                },
            ]
        );
    }

    #[test]
    fn text_before_tool_call_order() {
        let mut acc = Acc::new("m".into());
        acc.append_text("let me use a tool");
        acc.start_tool_call("id1".into(), "search".into());
        acc.tool_calls[0].arguments.push_str("{\"q\":");
        acc.tool_calls[0].arguments.push_str("\"rust\"}");
        let msg = acc.build_message();
        assert_eq!(
            msg.content,
            vec![
                AssistantContent::Text {
                    text: "let me use a tool".into()
                },
                AssistantContent::ToolCall(ToolCall {
                    id: "id1".into(),
                    name: "search".into(),
                    arguments: "{\"q\":\"rust\"}".into(),
                }),
            ]
        );
    }

    #[test]
    fn multiple_tool_calls_arguments_accumulate() {
        let mut acc = Acc::new("m".into());
        acc.start_tool_call("a".into(), "tool_a".into());
        acc.start_tool_call("b".into(), "tool_b".into());
        // 按 index 累积（与生产代码一致）。
        acc.tool_calls[1].arguments.push_str("{\"x\":1}");
        acc.tool_calls[0].arguments.push_str("{\"y\":2}");
        acc.tool_calls[1].arguments.push_str("");
        let msg = acc.build_message();
        let calls: Vec<ToolCall> = msg
            .content
            .iter()
            .filter_map(|c| match c {
                AssistantContent::ToolCall(tc) => Some(tc.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "a");
        assert_eq!(calls[0].arguments, "{\"y\":2}");
        assert_eq!(calls[1].id, "b");
        assert_eq!(calls[1].arguments, "{\"x\":1}");
    }

    #[test]
    fn end_tool_call_marks_done() {
        let mut acc = Acc::new("m".into());
        let idx = acc.start_tool_call("id1".into(), "t".into());
        assert!(!acc.tool_calls[idx].done);
        acc.end_tool_call("id1");
        assert!(acc.tool_calls[idx].done);
    }

    #[test]
    fn block_kind_tracking() {
        let mut acc = Acc::new("m".into());
        let tc_idx = acc.start_tool_call("id1".into(), "t".into());
        acc.note_block(0, SegmentKind::Text);
        acc.note_block(1, SegmentKind::ToolCall(tc_idx));
        assert!(matches!(acc.block_kind(0), Some(SegmentKind::Text)));
        assert!(matches!(acc.block_kind(1), Some(SegmentKind::ToolCall(0))));
        assert!(acc.block_kind(5).is_none());
    }

    #[test]
    fn build_message_carries_usage_stop_reason_model() {
        let mut acc = Acc::new("gpt-x".into());
        acc.append_text("hi");
        acc.usage = Some(Usage {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            total_tokens: 15,
            cost: 0.0,
        });
        acc.stop_reason = Some(StopReason::Completed);
        let msg = acc.build_message();
        assert_eq!(msg.model, Some(ModelId("gpt-x".into())));
        assert_eq!(msg.stop_reason, Some(StopReason::Completed));
        assert!(msg.usage.is_some());
        assert_eq!(msg.usage.as_ref().unwrap().total_tokens, 15);
        assert!(msg.error_message.is_none());
    }

    #[test]
    fn empty_acc_produces_empty_content() {
        let acc = Acc::new("m".into());
        let msg = acc.build_message();
        assert!(msg.content.is_empty());
        assert!(msg.usage.is_none());
        assert!(msg.stop_reason.is_none());
    }
}
