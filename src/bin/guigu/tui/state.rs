//! TUI 渲染状态 + 事件映射（Task 023）。
//!
//! `TuiState` 是纯数据（无 IO/锁），`apply_event` 是纯同步函数：
//! `AgentEvent` → `TuiState` 增量更新。全部可 `#[cfg(test)]` 单测（喂事件序列
//! 断言最终状态），不依赖终端。TUI 是纯渲染层：订阅事件流 + 读快照，不持有
//! agent 状态，不改 001/003 的「单写者」契约。

use guigu::core::event::AgentEvent;
use guigu::core::message::{Message, ToolResultContent, Usage, UserContent, UserMessage};
use guigu::core::provider::AssistantEvent;
use guigu::core::tool::ToolResult;

/// 工具卡片输出/参数显示的最大字符数（截断，避免撑爆布局）。
const MAX_OUTPUT: usize = 400;

/// 运行状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// 空闲（无 active run）。
    Idle,
    /// 运行中（active run 进行中）。
    Running,
    /// 错误（最近一次 run 失败）。
    Error,
}

/// 流式气泡（当前 assistant 增量，未落定）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamingBubble {
    /// 累积文本。
    pub text: String,
    /// 累积思考（折叠显示「思考中…」）。
    pub thinking: String,
}

/// 工具卡片状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCardStatus {
    /// 运行中（尚无输出）。
    Running,
    /// 运行中（有增量输出）。
    RunningPartial,
    /// 完成。
    Done,
    /// 失败。
    Failed,
}

/// 工具卡片（内联，含状态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCard {
    /// 工具调用 id（与 `ToolExecution*` 事件匹配）。
    pub id: String,
    /// 工具名。
    pub name: String,
    /// 参数（JSON 字符串，显示用）。
    pub args: String,
    /// 状态。
    pub status: ToolCardStatus,
    /// 结果/进度文本（截断）。
    pub output: String,
}

/// 对话区条目（时序：user/assistant 气泡 + 工具卡片交错）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConvItem {
    /// user 气泡。
    User { text: String },
    /// assistant 气泡（已落定）。
    Assistant { text: String },
    /// 工具卡片（内联，含状态）。
    Tool(ToolCard),
}

/// TUI 渲染状态（纯数据，无 IO/锁）。
///
/// 不含 `Eq`（`Usage.cost` 为 `f64`，非 `Eq`）。
#[derive(Debug, Clone, PartialEq)]
pub struct TuiState {
    /// 模型名（状态栏）。
    pub model: String,
    /// lane id（状态栏）。
    pub lane: String,
    /// 对话区条目（时序）。
    pub conv: Vec<ConvItem>,
    /// 当前 assistant 流式气泡（未落定）。
    pub streaming: Option<StreamingBubble>,
    /// 输入 buffer。
    pub input: String,
    /// 输入历史。
    pub history: Vec<String>,
    /// 历史游标（`usize::MAX` = 当前输入，非浏览历史）。
    pub history_cursor: usize,
    /// 滚动偏移（距底部行数，0 = 贴底）。
    pub scroll: usize,
    /// 运行状态。
    pub status: Status,
    /// 最近一次 usage。
    pub usage: Option<Usage>,
    /// 错误信息（状态栏显示）。
    pub error: Option<String>,
    /// Ctrl-C 已请求中止（第二次 Ctrl-C 退出）。
    pub abort_requested: bool,
}

impl TuiState {
    /// 新建状态（idle，空对话）。
    pub fn new(model: String, lane: String) -> Self {
        Self {
            model,
            lane,
            conv: Vec::new(),
            streaming: None,
            input: String::new(),
            history: Vec::new(),
            history_cursor: usize::MAX,
            scroll: 0,
            status: Status::Idle,
            usage: None,
            error: None,
            abort_requested: false,
        }
    }

    /// 落定流式气泡为 assistant 气泡（有文本时），清空 streaming。
    pub fn finalize_streaming(&mut self) {
        if let Some(streaming) = self.streaming.take()
            && !streaming.text.is_empty()
        {
            self.conv.push(ConvItem::Assistant {
                text: streaming.text,
            });
            self.scroll = 0;
        }
    }

    /// 按 id 查找工具卡片（可变）。
    pub fn find_tool_card(&mut self, id: &str) -> Option<&mut ToolCard> {
        self.conv.iter_mut().find_map(|item| match item {
            ConvItem::Tool(card) if card.id == id => Some(card),
            _ => None,
        })
    }

    /// upsert 工具卡片：存在则更新（name/args/status），不存在则追加 Running 卡片。
    pub fn upsert_tool_card(&mut self, id: &str, name: &str, args: &str, status: ToolCardStatus) {
        if let Some(card) = self.find_tool_card(id) {
            card.name = name.to_string();
            card.args = args.to_string();
            card.status = status;
        } else {
            self.conv.push(ConvItem::Tool(ToolCard {
                id: id.to_string(),
                name: name.to_string(),
                args: args.to_string(),
                status,
                output: String::new(),
            }));
            self.scroll = 0;
        }
    }
}

/// 事件 → 状态映射（纯同步函数，无 IO/锁，可单测）。
///
/// 对齐 002/003 事件语义：user 气泡 / assistant 流式 / 工具卡片状态机 /
/// 落定 / 终态。
pub fn apply_event(state: &mut TuiState, event: &AgentEvent) {
    match event {
        AgentEvent::AgentStart => {
            state.status = Status::Running;
            state.error = None;
            state.abort_requested = false;
        }
        AgentEvent::AgentEnd { .. } => {
            state.finalize_streaming();
            state.status = Status::Idle;
            state.abort_requested = false;
        }
        AgentEvent::TurnStart => {}
        AgentEvent::TurnEnd { .. } => {
            state.finalize_streaming();
        }
        AgentEvent::MessageStart { message } => match message.as_ref() {
            Message::User(user) => {
                state.conv.push(ConvItem::User {
                    text: user_text(user),
                });
                state.scroll = 0;
            }
            Message::Assistant(_) => {
                // 新 assistant 消息（每 turn 一个）：重置流式气泡。
                state.streaming = None;
            }
            Message::ToolResult(_) => {}
        },
        AgentEvent::MessageUpdate {
            assistant_event, ..
        } => match assistant_event {
            AssistantEvent::TextDelta { text } => {
                state
                    .streaming
                    .get_or_insert_with(StreamingBubble::default)
                    .text
                    .push_str(text);
                state.scroll = 0;
            }
            AssistantEvent::ThinkingDelta { thinking } => {
                state
                    .streaming
                    .get_or_insert_with(StreamingBubble::default)
                    .thinking
                    .push_str(thinking);
            }
            AssistantEvent::ToolCallStart {
                id,
                name,
                arguments,
            } => {
                state.upsert_tool_card(id, name, arguments, ToolCardStatus::Running);
            }
            AssistantEvent::ToolCallDelta { .. }
            | AssistantEvent::ToolCallEnd { .. }
            | AssistantEvent::Done { .. } => {}
            AssistantEvent::Error { message, .. } => {
                state.error = Some(message.clone());
                state.status = Status::Error;
                state.finalize_streaming();
            }
        },
        AgentEvent::MessageEnd { message } => {
            if let Message::Assistant(assistant) = message.as_ref() {
                if let Some(usage) = &assistant.usage {
                    state.usage = Some(usage.clone());
                }
                state.finalize_streaming();
            }
        }
        AgentEvent::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args,
        } => {
            state.upsert_tool_card(
                tool_call_id,
                tool_name,
                &args.to_string(),
                ToolCardStatus::Running,
            );
        }
        AgentEvent::ToolExecutionUpdate {
            tool_call_id,
            partial,
            ..
        } => {
            if let Some(card) = state.find_tool_card(tool_call_id) {
                card.status = ToolCardStatus::RunningPartial;
                card.output = truncate(&tool_result_text(partial), MAX_OUTPUT);
            }
        }
        AgentEvent::ToolExecutionEnd {
            tool_call_id,
            result,
            is_error,
            ..
        } => {
            if let Some(card) = state.find_tool_card(tool_call_id) {
                card.status = if *is_error {
                    ToolCardStatus::Failed
                } else {
                    ToolCardStatus::Done
                };
                card.output = truncate(&tool_result_text(result), MAX_OUTPUT);
            }
        }
    }
}

/// 提取 user 消息文本（拼接所有 Text 部分）。
fn user_text(user: &UserMessage) -> String {
    user.content
        .iter()
        .filter_map(|c| match c {
            UserContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// 提取工具结果文本（拼接所有 Text 部分）。
fn tool_result_text(result: &ToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| match c {
            ToolResultContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// 截断到 `max` 字符（按 char，非 byte），超出追加省略号。
fn truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else {
        let head: String = chars[..max].iter().collect();
        format!("{head}…")
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
