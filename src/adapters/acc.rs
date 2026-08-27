//! 流累积状态（OpenAI / Anthropic 适配器共享）。
//!
//! 累积规则：
//! 1. 文本/思考**按 block 独立累积**：Anthropic 每个 `content_block_start` 新建一个
//!    block（多个同类型 block 不合并）；OpenAI 无 block index，使用单一
//!    text/thinking block（首次出现时创建）。
//! 2. `ToolCallStart` 追加新 `ToolCallAcc`；`ToolCallDelta` 累积 arguments；
//!    `ToolCallEnd` 标记该项完成。
//! 3. 流结束 → [`Acc::build_message`] 按 segment 顺序构造完整 `AssistantMessage`
//!    （segment 顺序 = block start 顺序 = Anthropic content_block index 升序 /
//!    OpenAI 首次出现顺序）。

use crate::core::message::{
    AssistantContent, AssistantMessage, ModelId, StopReason, ToolCall, Usage,
};
use crate::core::provider::ProviderError;

/// 内容段种类（决定最终 content 顺序；载荷为对应累积数组的下标）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum SegmentKind {
    /// 文本段（下标指向 [`Acc::texts`]）。
    Text(usize),
    /// 思考段（下标指向 [`Acc::thinkings`]）。
    Thinking(usize),
    /// 工具调用段（下标指向 [`Acc::tool_calls`]）。
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
    /// 文本 block（每个 Anthropic text block 一个；OpenAI 仅一个）。
    pub texts: Vec<String>,
    /// 思考 block（每个 Anthropic thinking block 一个；OpenAI 仅一个）。
    pub thinkings: Vec<String>,
    /// 工具调用累积（按 start 顺序）。
    pub tool_calls: Vec<ToolCallAcc>,
    /// 内容段顺序（block start 顺序 / 首次出现顺序）。
    pub segments: Vec<SegmentKind>,
    /// Anthropic content_block index → 段种类（delta/stop 时定位段）。
    pub block_kinds: Vec<Option<SegmentKind>>,
    /// OpenAI provider 工具调用 index → 本地 `tool_calls` 下标
    /// （显式映射；provider index 可能非连续）。
    pub tool_index_map: Vec<Option<usize>>,
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
            texts: Vec::new(),
            thinkings: Vec::new(),
            tool_calls: Vec::new(),
            segments: Vec::new(),
            block_kinds: Vec::new(),
            tool_index_map: Vec::new(),
            usage: None,
            stop_reason: None,
            model,
            input_tokens: 0,
        }
    }

    /// OpenAI：追加文本增量（单一文本 block，首次出现时创建）。
    pub fn append_text(&mut self, text: &str) {
        let existing = self.segments.iter().find_map(|s| match s {
            SegmentKind::Text(i) => Some(*i),
            _ => None,
        });
        let idx = match existing {
            Some(i) => i,
            None => {
                let i = self.texts.len();
                self.texts.push(String::new());
                self.segments.push(SegmentKind::Text(i));
                i
            }
        };
        self.texts[idx].push_str(text);
    }

    /// OpenAI：追加思考增量（单一思考 block，首次出现时创建）。
    pub fn append_thinking(&mut self, thinking: &str) {
        let existing = self.segments.iter().find_map(|s| match s {
            SegmentKind::Thinking(i) => Some(*i),
            _ => None,
        });
        let idx = match existing {
            Some(i) => i,
            None => {
                let i = self.thinkings.len();
                self.thinkings.push(String::new());
                self.segments.push(SegmentKind::Thinking(i));
                i
            }
        };
        self.thinkings[idx].push_str(thinking);
    }

    /// Anthropic：新建文本 block（`content_block_start` 时调用），返回段下标。
    pub fn start_text_block(&mut self) -> usize {
        let i = self.texts.len();
        self.texts.push(String::new());
        self.segments.push(SegmentKind::Text(i));
        i
    }

    /// Anthropic：新建思考 block（`content_block_start` 时调用），返回段下标。
    pub fn start_thinking_block(&mut self) -> usize {
        let i = self.thinkings.len();
        self.thinkings.push(String::new());
        self.segments.push(SegmentKind::Thinking(i));
        i
    }

    /// Anthropic：向指定文本 block 追加增量。
    pub fn append_text_block(&mut self, seg_idx: usize, text: &str) {
        self.texts[seg_idx].push_str(text);
    }

    /// Anthropic：向指定思考 block 追加增量。
    pub fn append_thinking_block(&mut self, seg_idx: usize, thinking: &str) {
        self.thinkings[seg_idx].push_str(thinking);
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

    /// OpenAI：登记 provider index → 本地工具调用映射（start 时调用）。
    ///
    /// 同一 provider index 已被占用时返回 `Parse`（协议违规，禁止静默覆盖）。
    pub fn map_tool_index(
        &mut self,
        provider_index: usize,
        local_index: usize,
    ) -> Result<(), ProviderError> {
        if self.tool_index_map.len() <= provider_index {
            self.tool_index_map.resize(provider_index + 1, None);
        }
        match self.tool_index_map[provider_index] {
            None => {
                self.tool_index_map[provider_index] = Some(local_index);
                Ok(())
            }
            Some(_) => Err(ProviderError::Parse(format!(
                "duplicate tool_call start at provider index {provider_index}"
            ))),
        }
    }

    /// OpenAI：按 provider index 查本地 `tool_calls` 下标。
    pub fn tool_local_index(&self, provider_index: usize) -> Option<usize> {
        self.tool_index_map.get(provider_index).copied().flatten()
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
                SegmentKind::Text(i) => {
                    if let Some(text) = self.texts.get(*i)
                        && !text.is_empty()
                    {
                        content.push(AssistantContent::Text { text: text.clone() });
                    }
                }
                SegmentKind::Thinking(i) => {
                    if let Some(text) = self.thinkings.get(*i)
                        && !text.is_empty()
                    {
                        content.push(AssistantContent::Thinking { text: text.clone() });
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
mod tests;
