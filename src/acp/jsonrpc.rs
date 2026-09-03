//! JSON-RPC 2.0 wire 类型与入站消息分类（Task 014）。
//!
//! 从 `transport.rs` 拆出（单文件 ≤ 400 行约束）。含：
//! - `JsonRpcErrorObj` / `RequestId`：错误对象与请求 id（string / number）。
//! - `InboundMessage` / `OutboundMessage`：入站 / 出站消息。
//! - `InboundKind` / `classify_inbound`：入站消息校验与分类。
//!
//! 校验规则（`classify_inbound`）：
//! - `jsonrpc` **必须存在**且为 `"2.0"`（缺失 / 非法 → `-32600`）；
//! - 含 `method` 时不得同时含 `result` / `error`；
//! - 无 `method`（应答）时须有合法 `id` 且恰含 `result` / `error` 之一。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC 错误对象。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcErrorObj {
    /// 错误码（`-32603` = Internal error）。
    pub code: i64,
    /// 错误消息。
    pub message: String,
    /// 附加数据（可选）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// JSON-RPC 请求 id（string 或 number）。
///
/// JSON-RPC 2.0 允许 id 为 string / number / null。guigu 只接受 string / number
/// 作为 pending key（`null` 仅用于错误应答，不作为 key）。number 用 `i64` 承载
/// （含负数），使合法的字符串 id / 负数 id 都能正确路由，而非被静默忽略。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RequestId {
    /// 数字 id（含负数）。
    Number(i64),
    /// 字符串 id。
    String(String),
}

impl RequestId {
    /// 从 JSON-RPC `id` 值解析（仅 string / number 合法；其余 → `None`）。
    pub fn from_value(v: &Value) -> Option<Self> {
        if let Some(n) = v.as_i64() {
            Some(RequestId::Number(n))
        } else {
            v.as_str().map(|s| RequestId::String(s.to_string()))
        }
    }

    /// 转回 JSON-RPC `id` 值（用于出站请求 / 应答）。
    pub fn to_value(&self) -> Value {
        match self {
            RequestId::Number(n) => Value::from(*n),
            RequestId::String(s) => Value::String(s.clone()),
        }
    }
}

/// 入站 JSON-RPC 消息（client→agent）：请求 / notification / 应答。
///
/// 有 `method` → 请求（有 `id`）或 notification（无 `id`）；无 `method` 且有 `id`
/// → 对 agent 请求的应答。
#[derive(Debug, Clone, Deserialize)]
pub struct InboundMessage {
    /// JSON-RPC 版本（须为 `"2.0"`，由 `classify_inbound` 校验；缺失 → 非法）。
    #[serde(default)]
    pub jsonrpc: Option<String>,
    /// 关联号；notification 缺省。
    pub id: Option<Value>,
    /// 方法名；应答缺省。
    pub method: Option<String>,
    /// 请求参数。
    pub params: Option<Value>,
    /// 应答结果。
    pub result: Option<Value>,
    /// 应答错误。
    pub error: Option<JsonRpcErrorObj>,
}

/// 出站 JSON-RPC 消息（agent→client）：请求 / notification / 应答。
#[derive(Debug, Clone, Serialize)]
pub struct OutboundMessage {
    pub jsonrpc: String,
    /// 关联号；notification 缺省。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// 方法名；应答缺省。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// 请求参数。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    /// 应答结果。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// 应答错误。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcErrorObj>,
}

impl OutboundMessage {
    /// 构造对 client 请求的应答（成功）。
    pub fn result(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id),
            method: None,
            params: None,
            result: Some(result),
            error: None,
        }
    }

    /// 构造对 client 请求的应答（错误）。
    pub fn error(id: Value, code: i64, message: String) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id),
            method: None,
            params: None,
            result: None,
            error: Some(JsonRpcErrorObj {
                code,
                message,
                data: None,
            }),
        }
    }

    /// 构造 agent→client 请求（期望应答）。
    pub fn request(id: &RequestId, method: &str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id.to_value()),
            method: Some(method.into()),
            params: Some(params),
            result: None,
            error: None,
        }
    }

    /// 构造 agent→client notification（无应答）。
    pub fn notification(method: &str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: None,
            method: Some(method.into()),
            params: Some(params),
            result: None,
            error: None,
        }
    }
}

/// 校验后的入站消息分类。
#[derive(Debug)]
pub(crate) enum InboundKind {
    /// 请求（有 `method` + `id`）：spawn 处理并回 JSON-RPC 应答。
    Request {
        /// 关联号（原样回显）。
        id: Value,
        /// 方法名。
        method: String,
        /// 请求参数。
        params: Value,
    },
    /// notification（有 `method`，无 `id`）：spawn 处理，不应答。
    Notification {
        /// 方法名。
        method: String,
        /// 请求参数。
        params: Value,
    },
    /// 应答（无 `method`，有合法 `id`）：路由到 pending 请求。
    Response {
        /// 关联号（完整 JSON-RPC id）。
        id: RequestId,
        /// 应答结果（`Ok` = result，`Err` = error）。
        result: Result<Value, crate::acp::AcpError>,
    },
}

/// 校验并分类一条入站 JSON-RPC 2.0 消息。
///
/// 返回 `Ok(kind)` 表示合法；`Err((id, code, message))` 表示非法，调用方应回
/// `OutboundMessage::error(id, code, message)`（`id` 为原消息的合法 id 或 `null`）。
///
/// 校验规则：
/// - `jsonrpc` **必须存在**且为 `"2.0"`（缺失 / 非法 → `-32600`）；
/// - 含 `method` 时不得同时含 `result` / `error`；
/// - 无 `method`（应答）时须有合法 `id` 且恰含 `result` / `error` 之一。
pub(crate) fn classify_inbound(msg: &InboundMessage) -> Result<InboundKind, (Value, i64, String)> {
    // 1. 校验 jsonrpc 版本（必须存在且为 "2.0"）。
    match &msg.jsonrpc {
        Some(v) if v == "2.0" => {}
        Some(v) => {
            return Err((
                Value::Null,
                -32600,
                format!("invalid jsonrpc version: {v} (expected \"2.0\")"),
            ));
        }
        None => {
            return Err((
                Value::Null,
                -32600,
                "missing jsonrpc version (expected \"2.0\")".into(),
            ));
        }
    }

    let has_result = msg.result.is_some();
    let has_error = msg.error.is_some();

    if let Some(method) = &msg.method {
        // 请求 / notification。
        if has_result || has_error {
            return Err((
                msg.id.clone().unwrap_or(Value::Null),
                -32600,
                "invalid request: both method and result/error present".into(),
            ));
        }
        let params = msg.params.clone().unwrap_or(Value::Null);
        match &msg.id {
            Some(id) => Ok(InboundKind::Request {
                id: id.clone(),
                method: method.clone(),
                params,
            }),
            None => Ok(InboundKind::Notification {
                method: method.clone(),
                params,
            }),
        }
    } else {
        // 应答：须有合法 id + 恰含 result / error 之一。
        if has_result && has_error {
            return Err((
                msg.id.clone().unwrap_or(Value::Null),
                -32600,
                "invalid response: both result and error present".into(),
            ));
        }
        if !has_result && !has_error {
            return Err((
                msg.id.clone().unwrap_or(Value::Null),
                -32600,
                "invalid response: missing result and error".into(),
            ));
        }
        let id = msg
            .id
            .as_ref()
            .and_then(RequestId::from_value)
            .ok_or_else(|| {
                (
                    Value::Null,
                    -32600,
                    "invalid response: missing or invalid id".to_string(),
                )
            })?;
        let result = match (&msg.result, &msg.error) {
            (Some(result), None) => Ok(result.clone()),
            (None, Some(error)) => Err(crate::acp::AcpError::JsonRpc(error.message.clone())),
            _ => unreachable!("checked above"),
        };
        Ok(InboundKind::Response { id, result })
    }
}
