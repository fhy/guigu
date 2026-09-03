//! ACP v1 wire 类型（最小可用面）。
//!
//! 字段名 / 枚举值以 ACP v1 stable 官方规范为 wire 权威（camelCase）。guigu 只
//! 实现最小面：`initialize` 能力声明、`session/update` 变体、`ContentBlock`
//! （prompt 输入 / 输出）、`session/request_permission` 结果。
//!
//! 说明：ACP 的 `agentCapabilities` 不含 `fs` / `background` 字段（`fs` 是
//! **client** 能力，由 client 在 `initialize` 请求中声明）；本模块按官方 spec
//! 声明 `loadSession` + `promptCapabilities`，不臆造字段。

use serde::{Deserialize, Serialize};

/// ACP 协议主版本号（`protocolVersion`）。
pub const PROTOCOL_VERSION: u64 = 1;

/// guigu agent 标识（`agentInfo`）。
pub const AGENT_NAME: &str = "guigu";
pub const AGENT_TITLE: &str = "guigu";
pub const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// `promptCapabilities`：agent 支持的 prompt 内容类型。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCapabilities {
    /// 支持图片输入。
    pub image: bool,
    /// 支持音频输入。
    pub audio: bool,
    /// 支持内嵌上下文（resource / resource_link）。
    pub embedded_context: bool,
}

/// `agentCapabilities`：`initialize` 响应中的能力声明。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    /// 支持 `session/load`（从持久化恢复）。
    pub load_session: bool,
    /// prompt 内容能力。
    pub prompt_capabilities: PromptCapabilities,
}

impl AgentCapabilities {
    /// guigu 一期能力：支持 loadSession；prompt 仅文本（image/audio/embedded 均 false）。
    pub fn guigu() -> Self {
        Self {
            load_session: true,
            prompt_capabilities: PromptCapabilities {
                image: false,
                audio: false,
                embedded_context: false,
            },
        }
    }
}

/// `ContentBlock`：prompt 输入 / `session/update` 输出的内容块（最小面）。
///
/// 仅实现 guigu 用到的 `text` 变体；其余变体（image/audio/resource/
/// resource_link）反序列化时忽略（一期 prompt 仅文本）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// 文本块。
    Text {
        /// 文本内容。
        text: String,
    },
}

impl ContentBlock {
    /// 取文本内容（非文本块返回 `None`）。
    pub fn text(&self) -> Option<&str> {
        match self {
            ContentBlock::Text { text } => Some(text),
        }
    }
}

/// ACP `stopReason` 枚举（`session/prompt` 响应）。
///
/// 官方值：`end_turn` / `max_tokens` / `max_turn_requests` / `refusal` /
/// `cancelled`。guigu 内部 `StopReason` 的映射见 `mapping::acp_stop_reason`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpStopReason {
    /// agent 正常结束本轮。
    EndTurn,
    /// 达到 token 上限。
    MaxTokens,
    /// agent 拒绝。
    Refusal,
    /// 用户取消。
    Cancelled,
}

/// `session/request_permission` 的结果（`outcome`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionOutcome {
    /// client 选中某选项（`optionId`）。
    Selected { option_id: String },
    /// client 取消（未授权）。
    Cancelled,
}

impl PermissionOutcome {
    /// 是否授权（选中 `allow_*` 选项）。
    pub fn allowed(&self) -> bool {
        matches!(
            self,
            PermissionOutcome::Selected { option_id }
                if option_id == "allow_once" || option_id == "allow_always"
        )
    }

    /// 从 `session/request_permission` 的 JSON-RPC 结果解析 `outcome`。
    ///
    /// 结果结构为 `{ outcome: { outcome: "selected" | "cancelled", optionId? } }`
    /// （ACP `RequestPermissionResult`）。缺省 / 非法 → `Cancelled`。
    pub fn from_value(value: &serde_json::Value) -> Self {
        let outcome = value.get("outcome");
        match outcome
            .and_then(|o| o.get("outcome"))
            .and_then(serde_json::Value::as_str)
        {
            Some("selected") => {
                let option_id = outcome
                    .and_then(|o| o.get("optionId"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string();
                PermissionOutcome::Selected { option_id }
            }
            _ => PermissionOutcome::Cancelled,
        }
    }
}
