//! Session 协议 wire（Task 013）：`ServerRequest` / `ServerMessage`。
//!
//! 沿用 010 的 newline-delimited JSON 帧（复用 `crate::remote::codec`），命令面
//! 在 010 哲学上扩展 session/lane 寻址。`id` 单调递增由客户端分配，应答同 `id`。
//!
//! `Response.result` 为 `Result<serde_json::Value, String>`：`Ok` 负载因方法而异
//! （如 `CreateSession` 返回分配的 `session_id`），`Err` 为 `ServerError` 字符串化。
//! 因 `serde_json::Value` 可反序列化任意 JSON，内建 `Result` 的 `Deserialize`
//! 会把 `Err` 误读为 `Ok`，故用 `result_value_string` 自定义对称序列化
//! （`Ok(v)` ↔ `{"ok": v}`，`Err(s)` ↔ `{"err": s}`，无歧义）。

use serde::{Deserialize, Serialize};

use crate::core::agent::AgentSnapshot;
use crate::core::event::AgentEvent;
use crate::core::message::Message;
use crate::core::session::LaneId;
use crate::server::SessionId;

/// `Result<serde_json::Value, String>` 的对称 serde 序列化。
///
/// `Ok(v)` ↔ `{"ok": v}`，`Err(s)` ↔ `{"err": s}`。因 `Value` 可反序列化任意
/// JSON，内建 `Result` 的 `Deserialize` 会把 `Err` 误读为 `Ok`，故自定义以保证
/// wire 格式可往返且无歧义。
mod result_value_string {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use serde_json::Value;

    pub fn serialize<S>(value: &Result<Value, String>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Ok(v) => {
                let tagged = serde_json::json!({ "ok": v });
                tagged.serialize(serializer)
            }
            Err(s) => {
                let tagged = serde_json::json!({ "err": s });
                tagged.serialize(serializer)
            }
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Result<Value, String>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let v = Value::deserialize(deserializer)?;
        if let Some(obj) = v.as_object() {
            if let Some(inner) = obj.get("ok") {
                return Ok(Ok(inner.clone()));
            }
            if let Some(s) = obj.get("err").and_then(Value::as_str) {
                return Ok(Err(s.to_string()));
            }
        }
        Err(serde::de::Error::custom(
            "expected {\"ok\": ...} or {\"err\": ...}",
        ))
    }
}

/// 客户端 → 服务端。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerRequest {
    /// 新建空 session；`session_id = None` 时由服务端分配。
    CreateSession {
        id: u64,
        session_id: Option<SessionId>,
    },
    /// 从持久化存储 load + reduce 重建 session（崩溃恢复入口）。
    LoadSession { id: u64, session_id: SessionId },
    /// 列出全部 session。
    ListSessions { id: u64 },
    /// 在 session 内 spawn 一个 lane（runtime 由服务端工厂构造）。
    SpawnLane {
        id: u64,
        session_id: SessionId,
        lane_id: LaneId,
    },
    /// 从 `from_lane` 分支出新 lane。
    ForkLane {
        id: u64,
        session_id: SessionId,
        from_lane: LaneId,
        new_lane: LaneId,
    },
    /// 向 lane 发送提示消息。
    Prompt {
        id: u64,
        session_id: SessionId,
        lane_id: LaneId,
        messages: Vec<Message>,
    },
    /// 继续处理。
    Continue {
        id: u64,
        session_id: SessionId,
        lane_id: LaneId,
    },
    /// 中止当前操作。
    Abort {
        id: u64,
        session_id: SessionId,
        lane_id: LaneId,
    },
    /// 重置 lane（清空 transcript 与队列）。
    Reset {
        id: u64,
        session_id: SessionId,
        lane_id: LaneId,
    },
    /// 请求 lane 当前快照。
    GetSnapshot {
        id: u64,
        session_id: SessionId,
        lane_id: LaneId,
    },
    /// 订阅 lane 事件（服务端据此推送 `Event`）。
    Subscribe {
        id: u64,
        session_id: SessionId,
        lane_id: LaneId,
    },
    /// 取消订阅 lane 事件。
    Unsubscribe {
        id: u64,
        session_id: SessionId,
        lane_id: LaneId,
    },
    /// 关闭 server（所有 session / lane）。
    Shutdown { id: u64 },
}

impl ServerRequest {
    /// 请求关联号（所有变体均携带）。
    pub fn id(&self) -> u64 {
        match self {
            ServerRequest::CreateSession { id, .. }
            | ServerRequest::LoadSession { id, .. }
            | ServerRequest::ListSessions { id }
            | ServerRequest::SpawnLane { id, .. }
            | ServerRequest::ForkLane { id, .. }
            | ServerRequest::Prompt { id, .. }
            | ServerRequest::Continue { id, .. }
            | ServerRequest::Abort { id, .. }
            | ServerRequest::Reset { id, .. }
            | ServerRequest::GetSnapshot { id, .. }
            | ServerRequest::Subscribe { id, .. }
            | ServerRequest::Unsubscribe { id, .. }
            | ServerRequest::Shutdown { id } => *id,
        }
    }
}

/// 服务端 → 客户端。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// 对命令的应答；`result` 为 `Ok(负载)` / `Err(错误字符串)`。
    Response {
        id: u64,
        #[serde(with = "result_value_string")]
        result: Result<serde_json::Value, String>,
    },
    /// 对 `ListSessions` 的应答。
    SessionList { id: u64, sessions: Vec<SessionId> },
    /// 对 `GetSnapshot` 的应答（带 session/lane 前缀供客户端路由）。
    Snapshot {
        session_id: SessionId,
        lane_id: LaneId,
        snapshot: AgentSnapshot,
    },
    /// 服务端推送的 lane 事件（带 session/lane 前缀供客户端路由）。
    Event {
        session_id: SessionId,
        lane_id: LaneId,
        event: AgentEvent,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::message::{ThinkingLevel, UserContent, UserMessage};

    fn user_msg(text: &str) -> Message {
        Message::User(UserMessage {
            content: vec![UserContent::Text {
                text: text.to_string(),
            }],
            timestamp: 0,
        })
    }

    fn all_requests() -> Vec<ServerRequest> {
        vec![
            ServerRequest::CreateSession {
                id: 1,
                session_id: Some("s1".to_string()),
            },
            ServerRequest::CreateSession {
                id: 2,
                session_id: None,
            },
            ServerRequest::LoadSession {
                id: 3,
                session_id: "s1".to_string(),
            },
            ServerRequest::ListSessions { id: 4 },
            ServerRequest::SpawnLane {
                id: 5,
                session_id: "s1".to_string(),
                lane_id: "l1".to_string(),
            },
            ServerRequest::ForkLane {
                id: 6,
                session_id: "s1".to_string(),
                from_lane: "l1".to_string(),
                new_lane: "l2".to_string(),
            },
            ServerRequest::Prompt {
                id: 7,
                session_id: "s1".to_string(),
                lane_id: "l1".to_string(),
                messages: vec![user_msg("hi")],
            },
            ServerRequest::Continue {
                id: 8,
                session_id: "s1".to_string(),
                lane_id: "l1".to_string(),
            },
            ServerRequest::Abort {
                id: 9,
                session_id: "s1".to_string(),
                lane_id: "l1".to_string(),
            },
            ServerRequest::Reset {
                id: 10,
                session_id: "s1".to_string(),
                lane_id: "l1".to_string(),
            },
            ServerRequest::GetSnapshot {
                id: 11,
                session_id: "s1".to_string(),
                lane_id: "l1".to_string(),
            },
            ServerRequest::Subscribe {
                id: 12,
                session_id: "s1".to_string(),
                lane_id: "l1".to_string(),
            },
            ServerRequest::Unsubscribe {
                id: 13,
                session_id: "s1".to_string(),
                lane_id: "l1".to_string(),
            },
            ServerRequest::Shutdown { id: 14 },
        ]
    }

    /// `ServerRequest` 全变体序列化 roundtrip。
    #[test]
    fn test_server_request_roundtrip() {
        for req in all_requests() {
            let json = serde_json::to_string(&req).expect("serialize");
            let decoded: ServerRequest = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(decoded, req, "roundtrip mismatch for {json}");
        }
    }

    /// `id` 访问器返回各变体的关联号。
    #[test]
    fn test_server_request_id_accessor() {
        for req in all_requests() {
            let expected = match &req {
                ServerRequest::CreateSession { id, .. }
                | ServerRequest::LoadSession { id, .. }
                | ServerRequest::ListSessions { id }
                | ServerRequest::SpawnLane { id, .. }
                | ServerRequest::ForkLane { id, .. }
                | ServerRequest::Prompt { id, .. }
                | ServerRequest::Continue { id, .. }
                | ServerRequest::Abort { id, .. }
                | ServerRequest::Reset { id, .. }
                | ServerRequest::GetSnapshot { id, .. }
                | ServerRequest::Subscribe { id, .. }
                | ServerRequest::Unsubscribe { id, .. }
                | ServerRequest::Shutdown { id } => *id,
            };
            assert_eq!(req.id(), expected);
        }
    }

    /// 序列化使用 snake_case 标签（`type` 字段）。
    #[test]
    fn test_server_request_snake_case_tag() {
        let json =
            serde_json::to_string(&ServerRequest::ListSessions { id: 0 }).expect("serialize");
        assert!(
            json.contains("\"type\":\"list_sessions\""),
            "expected snake_case tag, got {json}"
        );
    }

    fn snapshot() -> AgentSnapshot {
        AgentSnapshot {
            system_prompt: "sys".to_string(),
            model: Some("m".to_string()),
            thinking_level: ThinkingLevel::Off,
            messages: vec![],
            is_streaming: false,
            streaming_message: None,
            pending_tool_calls: std::collections::HashSet::new(),
            error_message: None,
        }
    }

    /// `ServerMessage` 全变体 roundtrip（含 `Response` 的 Ok/Err 两态）。
    #[test]
    fn test_server_message_roundtrip() {
        let messages = vec![
            ServerMessage::Response {
                id: 1,
                result: Ok(serde_json::Value::Null),
            },
            ServerMessage::Response {
                id: 2,
                result: Ok(serde_json::json!("s1")),
            },
            ServerMessage::Response {
                id: 3,
                result: Err("session not found: s1".to_string()),
            },
            ServerMessage::SessionList {
                id: 4,
                sessions: vec!["s1".to_string(), "s2".to_string()],
            },
            ServerMessage::Snapshot {
                session_id: "s1".to_string(),
                lane_id: "l1".to_string(),
                snapshot: snapshot(),
            },
            ServerMessage::Event {
                session_id: "s1".to_string(),
                lane_id: "l1".to_string(),
                event: AgentEvent::AgentStart,
            },
        ];

        for msg in messages {
            let json = serde_json::to_string(&msg).expect("serialize");
            let decoded: ServerMessage = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(decoded, msg, "roundtrip mismatch for {json}");
        }
    }

    /// `Response` 的 Ok/Err 两态可区分（`{"ok":...}` / `{"err":...}`）。
    #[test]
    fn test_server_message_response_two_states() {
        let ok: ServerMessage =
            serde_json::from_str(r#"{"type":"response","id":1,"result":{"ok":"s1"}}"#)
                .expect("deserialize ok");
        match ok {
            ServerMessage::Response { result, .. } => {
                assert_eq!(result, Ok(serde_json::json!("s1")));
            }
            other => panic!("expected Response, got {other:?}"),
        }

        let err: ServerMessage =
            serde_json::from_str(r#"{"type":"response","id":2,"result":{"err":"boom"}}"#)
                .expect("deserialize err");
        match err {
            ServerMessage::Response { result, .. } => assert_eq!(result, Err("boom".to_string())),
            other => panic!("expected Response, got {other:?}"),
        }
    }

    /// `Ok(Null)` 与 `Err` 不混淆：`{"ok":null}` 反序列化为 `Ok(Null)`。
    #[test]
    fn test_server_message_response_ok_null() {
        let msg: ServerMessage =
            serde_json::from_str(r#"{"type":"response","id":1,"result":{"ok":null}}"#)
                .expect("deserialize");
        match msg {
            ServerMessage::Response { result, .. } => {
                assert_eq!(result, Ok(serde_json::Value::Null));
            }
            other => panic!("expected Response, got {other:?}"),
        }
    }
}
