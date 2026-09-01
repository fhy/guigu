//! Wire 协议类型：`RemoteRequest` / `ServerMessage` / `RemoteError`。
//!
//! 命令面与 001 `AgentCommand` 一一对应，不新增语义。`id` 为请求关联号，
//! 由客户端单调递增（`AtomicU64`），应答携带同 `id`；`id = 0` 保留给连接
//! 建立时服务端推送的初始 `Snapshot`。
//!
//! `Response.result` 为 `Result<(), String>`：`Ok(())` 表示命令被 runtime
//! 接受（与进程内一致，入队即返回，不等待 run 结束）；`Err(String)` 为
//! `AgentError` 的字符串化。

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::core::agent::AgentSnapshot;
use crate::core::event::AgentEvent;
use crate::core::message::Message;

/// `Result<(), String>` 的对称 serde 序列化：`Ok(())` ↔ `null`，`Err(s)` ↔ 字符串。
///
/// serde 内建 `Result` 的 `Serialize`/`Deserialize` 不对称（`Ok(())` 序列化为
/// `null` 但无法从 `null` 反序列化），故自定义以保证 wire 格式可往返。
mod result_unit_string {
    use serde::{Deserializer, Serializer};

    pub fn serialize<S>(value: &Result<(), String>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Ok(()) => serializer.serialize_none(),
            Err(s) => serializer.serialize_str(s),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Result<(), String>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Result<(), String>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("null or string")
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(Ok(()))
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(Err(v.to_string()))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

/// 客户端 → 服务端。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RemoteRequest {
    Prompt { id: u64, messages: Vec<Message> },
    Continue { id: u64 },
    Steer { id: u64, message: Message },
    FollowUp { id: u64, message: Message },
    Abort { id: u64 },
    Reset { id: u64 },
    GetSnapshot { id: u64 },
    Shutdown { id: u64 },
}

impl RemoteRequest {
    /// 请求关联号（所有变体均携带）。
    pub fn id(&self) -> u64 {
        match self {
            RemoteRequest::Prompt { id, .. }
            | RemoteRequest::Continue { id }
            | RemoteRequest::Steer { id, .. }
            | RemoteRequest::FollowUp { id, .. }
            | RemoteRequest::Abort { id }
            | RemoteRequest::Reset { id }
            | RemoteRequest::GetSnapshot { id }
            | RemoteRequest::Shutdown { id } => *id,
        }
    }
}

/// 服务端 → 客户端。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// 对 `GetSnapshot` 的应答，以及连接建立时的初始快照（`id = 0`）。
    Snapshot { id: u64, snapshot: AgentSnapshot },
    /// 对命令（Prompt/Continue/Steer/FollowUp/Abort/Reset/Shutdown）的应答。
    Response {
        id: u64,
        #[serde(with = "result_unit_string")]
        result: Result<(), String>,
    },
    /// 服务端推送的 agent 事件。
    Event { event: AgentEvent },
}

/// 远程协议错误。
#[derive(Debug, Error)]
pub enum RemoteError {
    /// IO 错误。
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// 序列化错误。
    #[error("serialize error: {0}")]
    Serde(#[from] serde_json::Error),
    /// 协议层错误（坏帧、通道关闭等）。
    #[error("protocol error: {0}")]
    Protocol(String),
    /// 命令错误（服务端 `AgentError` 字符串化）。
    #[error("command error: {0}")]
    Command(String),
    /// 请求超时。
    #[error("request timeout")]
    Timeout,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::message::{UserContent, UserMessage};

    fn user_msg(text: &str) -> Message {
        Message::User(UserMessage {
            content: vec![UserContent::Text {
                text: text.to_string(),
            }],
            timestamp: 0,
        })
    }

    fn all_requests() -> Vec<RemoteRequest> {
        vec![
            RemoteRequest::Prompt {
                id: 1,
                messages: vec![user_msg("hi")],
            },
            RemoteRequest::Continue { id: 2 },
            RemoteRequest::Steer {
                id: 3,
                message: user_msg("steer"),
            },
            RemoteRequest::FollowUp {
                id: 4,
                message: user_msg("follow"),
            },
            RemoteRequest::Abort { id: 5 },
            RemoteRequest::Reset { id: 6 },
            RemoteRequest::GetSnapshot { id: 7 },
            RemoteRequest::Shutdown { id: 8 },
        ]
    }

    /// 全变体序列化 roundtrip。
    #[test]
    fn test_remote_request_roundtrip() {
        for req in all_requests() {
            let json = serde_json::to_string(&req).expect("serialize");
            let decoded: RemoteRequest = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(decoded, req, "roundtrip mismatch for {json}");
        }
    }

    /// `id` 访问器返回各变体的关联号。
    #[test]
    fn test_remote_request_id_accessor() {
        for req in all_requests() {
            let expected = match &req {
                RemoteRequest::Prompt { id, .. }
                | RemoteRequest::Continue { id }
                | RemoteRequest::Steer { id, .. }
                | RemoteRequest::FollowUp { id, .. }
                | RemoteRequest::Abort { id }
                | RemoteRequest::Reset { id }
                | RemoteRequest::GetSnapshot { id }
                | RemoteRequest::Shutdown { id } => *id,
            };
            assert_eq!(req.id(), expected);
        }
    }

    /// 序列化使用 snake_case 标签（`type` 字段）。
    #[test]
    fn test_remote_request_snake_case_tag() {
        let json = serde_json::to_string(&RemoteRequest::GetSnapshot { id: 0 }).expect("serialize");
        assert!(
            json.contains("\"type\":\"get_snapshot\""),
            "expected snake_case tag, got {json}"
        );
    }

    /// `ServerMessage` 全变体 roundtrip（含 `Response` 的 Ok/Err 两态）。
    #[test]
    fn test_server_message_roundtrip() {
        let snapshot = AgentSnapshot {
            system_prompt: "sys".to_string(),
            model: Some("m".to_string()),
            thinking_level: crate::core::message::ThinkingLevel::Off,
            messages: vec![],
            is_streaming: false,
            streaming_message: None,
            pending_tool_calls: std::collections::HashSet::new(),
            error_message: None,
        };

        let messages = vec![
            ServerMessage::Snapshot {
                id: 0,
                snapshot: snapshot.clone(),
            },
            ServerMessage::Response {
                id: 1,
                result: Ok(()),
            },
            ServerMessage::Response {
                id: 2,
                result: Err("boom".to_string()),
            },
            ServerMessage::Event {
                event: AgentEvent::AgentStart,
            },
            ServerMessage::Event {
                event: AgentEvent::TurnStart,
            },
        ];

        for msg in messages {
            let json = serde_json::to_string(&msg).expect("serialize");
            let decoded: ServerMessage = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(decoded, msg, "roundtrip mismatch for {json}");
        }
    }

    /// `Response` 的 Ok/Err 两态可区分。
    ///
    /// `Result<(), String>` 的 `Ok(())` 序列化为 `null`，`Err(s)` 序列化为字符串。
    #[test]
    fn test_server_message_response_two_states() {
        let ok: ServerMessage = serde_json::from_str(r#"{"type":"response","id":1,"result":null}"#)
            .expect("deserialize ok");
        match ok {
            ServerMessage::Response { result, .. } => assert!(result.is_ok()),
            other => panic!("expected Response, got {other:?}"),
        }

        let err: ServerMessage =
            serde_json::from_str(r#"{"type":"response","id":2,"result":"boom"}"#)
                .expect("deserialize err");
        match err {
            ServerMessage::Response { result, .. } => assert_eq!(result, Err("boom".to_string())),
            other => panic!("expected Response, got {other:?}"),
        }
    }
}
