use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    User(UserMessage),
    Assistant(AssistantMessage),
    ToolResult(ToolResultMessage),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserMessage {
    pub content: Vec<UserContent>,
    pub timestamp: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserContent {
    Text { text: String },
    Image(ImageContent),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageContent {
    pub data: String, // base64
    pub mime_type: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantMessage {
    pub content: Vec<AssistantContent>,
    pub model: Option<ModelId>,
    pub usage: Option<Usage>,
    pub stop_reason: Option<StopReason>,
    pub error_message: Option<String>,
    pub timestamp: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantContent {
    Text { text: String },
    Thinking { text: String },
    ToolCall(ToolCall),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String, // JSON 字符串（流式累积期可为部分）
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResultMessage {
    pub tool_call_id: String,
    pub tool_name: String,
    pub is_error: bool,
    pub content: Vec<ToolResultContent>,
    pub details: Option<serde_json::Value>,
    pub timestamp: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultContent {
    Text { text: String },
    Image(ImageContent),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelId(pub String);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total_tokens: u64,
    pub cost: f64,
}

// 自定义 Serialize/Deserialize 实现 StopReason 的未知值兜底功能
#[derive(Debug, Clone, PartialEq)]
pub enum StopReason {
    Completed,
    Length,
    Error,
    Aborted,
    Pending,
    Other(String),
}

impl Serialize for StopReason {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            StopReason::Completed => serializer.serialize_str("completed"),
            StopReason::Length => serializer.serialize_str("length"),
            StopReason::Error => serializer.serialize_str("error"),
            StopReason::Aborted => serializer.serialize_str("aborted"),
            StopReason::Pending => serializer.serialize_str("pending"),
            StopReason::Other(s) => serializer.serialize_str(s),
        }
    }
}

impl<'de> Deserialize<'de> for StopReason {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(StopReason::from_string(&s))
    }
}

impl StopReason {
    pub fn from_string(s: &str) -> Self {
        match s {
            "completed" => StopReason::Completed,
            "length" => StopReason::Length,
            "error" => StopReason::Error,
            "aborted" => StopReason::Aborted,
            "pending" => StopReason::Pending,
            _ => StopReason::Other(s.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}
