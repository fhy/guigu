//! ACP 适配（Task 014）：Agent Client Protocol v1 agent 侧。
//!
//! guigu 作为 **agent 侧**，实现 client→agent 方法（`initialize` / `session/*`），
//! 并经 `AcpClient` 句柄主动向 client 发起 agent→client 调用（`session/update`、
//! `fs/read_text_file`、`fs/write_text_file`、`session/request_permission`）。
//!
//! 模块拆分（单文件 ≤ 400 行约束）：
//! - `types`：ACP wire 类型（`ContentBlock` / `AcpStopReason` / `PermissionOutcome` 等）
//! - `mapping`：`AgentEvent` → `SessionUpdate`、`StopReason` → ACP `stopReason` 映射
//! - `handlers`：各 ACP 方法处理（`impl AcpAgent`）
//! - `jsonrpc`：JSON-RPC 2.0 wire 类型 + 入站消息校验 / 分类
//! - `stdio_client`：`StdioClient`（`AcpClient` 实现）+ `StdioConnection`（连接资源）
//! - `transport`：`serve_stdio` / `serve_connection`（stdio 连接循环 + writer task）
//! - `fs_tool`：`AcpFsTool`（fs 读写经 `AcpClient` 代理，实现 `Tool` trait）
//!
//! 边界声明（同任务规格）：一期不支持 authenticate（`authMethods: []`）；不做 MCP
//! 集成；不做 terminal 方法；SSE+HTTP 为可选加分项（`acp-sse` feature，本任务降级为
//! 后续）；工具装配由 015 CLI / 嵌入方经 `AgentServer` 工厂注入。

mod fs_tool;
mod handlers;
mod jsonrpc;
mod mapping;
mod stdio_client;
mod transport;
mod types;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_fs;
#[cfg(test)]
mod tests_transport;
#[cfg(test)]
mod testutil;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};

use crate::server::{AgentServer, ServerError};

pub use fs_tool::AcpFsTool;
pub use mapping::{acp_stop_reason, content_blocks_to_messages, map_event_to_update};
pub use stdio_client::{StdioClient, StdioConnection};
pub use types::{
    AcpStopReason, AgentCapabilities, ContentBlock, PROTOCOL_VERSION, PermissionOutcome,
    PromptCapabilities,
};

/// ACP 错误类型。
#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    /// JSON-RPC 层错误。
    #[error("json-rpc error: {0}")]
    JsonRpc(String),
    /// 不支持的方法。
    #[error("unsupported method: {0}")]
    UnsupportedMethod(String),
    /// 后端 `AgentServer` 错误。
    #[error("server error: {0}")]
    Server(#[from] ServerError),
    /// IO 错误。
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// 序列化错误。
    #[error("serialize error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// 权限模式（`session/set_mode` 的 `modeId`）。
///
/// 对齐 ACP 模式 id（camelCase）。`requires_permission` 决定工具执行前是否向
/// client 请求权限：`default` / `plan` 需要；`acceptEdits` / `bypassPermissions` /
/// `dontAsk` / `auto` 直接放行（后两者规格未定义行为，按「不询问」处理，见任务规格
/// 边界声明）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    /// 默认：工具执行前询问。
    Default,
    /// 自动接受编辑。
    AcceptEdits,
    /// 计划模式：工具执行前询问。
    Plan,
    /// 绕过权限检查。
    BypassPermissions,
    /// 不询问（直接放行）。
    DontAsk,
    /// 自动（直接放行）。
    Auto,
}

impl PermissionMode {
    /// 是否需要向 client 请求权限（`default` / `plan` 需要；其余直接放行）。
    pub fn requires_permission(&self) -> bool {
        matches!(self, PermissionMode::Default | PermissionMode::Plan)
    }

    /// 从 ACP `modeId` 字符串解析（未知 → `Default`）。
    pub fn from_mode_id(id: &str) -> Self {
        match id {
            "default" => PermissionMode::Default,
            "acceptEdits" => PermissionMode::AcceptEdits,
            "plan" => PermissionMode::Plan,
            "bypassPermissions" => PermissionMode::BypassPermissions,
            "dontAsk" => PermissionMode::DontAsk,
            "auto" => PermissionMode::Auto,
            _ => PermissionMode::Default,
        }
    }
}

/// 由传输层注入的「调用 client 方法」句柄（stdio / SSE 各自实现）。
///
/// agent 侧经此句柄向 client 发起请求（`request`，期望 JSON-RPC 结果）或
/// notification（`notify`，无应答）。
#[async_trait]
pub trait AcpClient: Send + Sync {
    /// 向 client 发请求（如 `fs/read_text_file`、`session/request_permission`），返回 JSON-RPC 结果。
    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, AcpError>;
    /// 向 client 发 notification（如 `session/update`）。
    async fn notify(&self, method: &str, params: serde_json::Value) -> Result<(), AcpError>;
}

/// ACP agent 侧适配器：实现 client→agent 方法，并经 `AcpClient` 句柄发起 agent→client 调用。
///
/// 复用 013 `AgentServer` 做 session / lane 后端；权限模式为 **session 级**
/// （`modes` 按 `sessionId` 隔离，`session/set_mode` 只影响目标 session，避免多
/// session 并发时互相串改权限）。
///
/// 双工要点：`handle` 收到 `session/prompt` 时，边消费 `AgentServer` 事件流边向
/// client 推 `session/update`，lane run 结束后返回 `PromptResponse { stopReason }`。
/// transport 为每条入站请求 spawn 独立 task，故 `session/prompt`（阻塞至 run 结束）
/// 与 `session/cancel`（中止 lane）可并发处理。
pub struct AcpAgent {
    /// 013 多 session 后端。
    server: AgentServer,
    /// session 级权限模式：`sessionId` → 模式句柄（`session/set_mode` 更新）。
    modes: Arc<Mutex<HashMap<String, Arc<RwLock<PermissionMode>>>>>,
}

impl AcpAgent {
    /// 创建 ACP 适配器（各 session 初始权限模式为 `Default`）。
    ///
    /// 嵌入方须先经 `server.with_runtime_factory` / `with_storage_factory` 配置工厂，
    /// 否则 `session/new` / `session/load` / `session/prompt` 会因工厂缺失返回错误。
    pub fn new(server: AgentServer) -> Self {
        Self {
            server,
            modes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 处理一条 client→agent JSON-RPC 请求（dispatch 到各 ACP 方法）。
    ///
    /// 返回 JSON-RPC `result` 负载；notification（如 `session/cancel` /
    /// `session/set_mode`）返回 `Ok(Value::Null)`，由 transport 决定是否发应答。
    pub async fn handle(
        &self,
        client: &dyn AcpClient,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, AcpError> {
        match method {
            "initialize" => self.handle_initialize(params).await,
            "authenticate" => self.handle_authenticate(params).await,
            "session/new" => self.handle_new_session(params).await,
            "session/load" => self.handle_load_session(params).await,
            "session/prompt" => self.handle_prompt(client, params).await,
            "session/cancel" => self.handle_cancel(params).await,
            "session/set_mode" => self.handle_set_mode(params).await,
            other => Err(AcpError::UnsupportedMethod(other.to_string())),
        }
    }

    /// 获取 session 的权限模式句柄（不存在则创建为 `Default`）。
    ///
    /// 供嵌入方创建 `AcpFsTool` 时绑定 **session 级** 权限模式；`session/set_mode`
    /// 更新同一句柄，故工具始终读到该 session 的最新模式，且不影响其他 session。
    pub async fn mode_for(&self, session_id: &str) -> Arc<RwLock<PermissionMode>> {
        let mut modes = self.modes.lock().await;
        modes
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(RwLock::new(PermissionMode::Default)))
            .clone()
    }

    /// 后端 `AgentServer`（供嵌入方 / 测试访问）。
    pub fn server(&self) -> &AgentServer {
        &self.server
    }
}
