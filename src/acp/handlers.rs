//! ACP 方法处理（`impl AcpAgent`）：client→agent 各方法的实现。
//!
//! 从 `mod.rs` 拆出（单文件 ≤ 400 行约束）。`handle`（dispatch）留在 `mod.rs`，
//! 各具体方法在此。`session/prompt` 为双工核心：订阅 lane 事件流 → 逐条映射为
//! `session/update` 推送 → run 结束后返回 `PromptResponse { stopReason }`。

use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::acp::mapping::{acp_stop_reason, content_blocks_to_messages, map_event_to_update};
use crate::acp::types::{
    AGENT_NAME, AGENT_TITLE, AGENT_VERSION, AgentCapabilities, ContentBlock, PROTOCOL_VERSION,
};
use crate::acp::{AcpAgent, AcpClient, AcpError, PermissionMode};
use crate::core::event::AgentEvent;
use crate::core::message::{Message, StopReason};
use crate::server::ServerError;

/// 默认 lane id（ACP 一期单 lane：`session/prompt` / `session/cancel` 均路由到它）。
const DEFAULT_LANE: &str = "default";

impl AcpAgent {
    /// `initialize`（c→a）：返回 `protocolVersion` + `AgentCapabilities` + `agentInfo`。
    ///
    /// 一期不支持 auth（`authMethods: []`）；不做 MCP（`mcpCapabilities` 缺省）。
    pub(crate) async fn handle_initialize(&self, _params: Value) -> Result<Value, AcpError> {
        Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "agentCapabilities": AgentCapabilities::guigu(),
            "agentInfo": {
                "name": AGENT_NAME,
                "title": AGENT_TITLE,
                "version": AGENT_VERSION
            },
            "authMethods": []
        }))
    }

    /// `authenticate`（c→a）：一期不支持 auth → 返回 `authMethods: []`（对齐任务规格
    /// 方法映射与边界声明；不能仅依赖 `initialize` 的空能力声明，兼容客户端可能在
    /// 初始化后仍调用该方法）。
    pub(crate) async fn handle_authenticate(&self, _params: Value) -> Result<Value, AcpError> {
        Ok(json!({ "authMethods": [] }))
    }

    /// `session/new`（c→a）：分配 sessionId → 建 session → spawn 默认 lane → 返回 `{ sessionId }`。
    pub(crate) async fn handle_new_session(&self, _params: Value) -> Result<Value, AcpError> {
        let session_id = self.server.allocate_session_id();
        self.server
            .create_session_from_factory(session_id.clone())
            .await?;
        self.server
            .spawn_lane_from_factory(&session_id, DEFAULT_LANE)
            .await?;
        Ok(json!({ "sessionId": session_id }))
    }

    /// `session/load`（c→a）：从持久化恢复 session → spawn 默认 lane → 返回 `{ sessionId }`。
    pub(crate) async fn handle_load_session(&self, params: Value) -> Result<Value, AcpError> {
        let session_id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| AcpError::JsonRpc("missing sessionId".into()))?
            .to_string();
        self.server
            .load_session_from_factory(session_id.clone())
            .await?;
        self.server
            .spawn_lane_from_factory(&session_id, DEFAULT_LANE)
            .await?;
        Ok(json!({ "sessionId": session_id }))
    }

    /// `session/prompt`（c→a）：双工核心。
    ///
    /// 1. 解析 `sessionId` + `prompt`（`ContentBlock[]`）→ guigu `Vec<Message>`；
    /// 2. 确保默认 lane 存在（不存在则 spawn）；
    /// 3. 订阅 lane 事件流；
    /// 4. 发送 prompt（入队即返回，run 在后台进行）；
    /// 5. 消费事件流：逐条映射为 `session/update` 推送；跟踪 `stopReason`；
    /// 6. `AgentEnd` 时返回 `PromptResponse { stopReason }`。
    pub(crate) async fn handle_prompt(
        &self,
        client: &dyn AcpClient,
        params: Value,
    ) -> Result<Value, AcpError> {
        let session_id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| AcpError::JsonRpc("missing sessionId".into()))?
            .to_string();
        let blocks = parse_content_blocks(
            params
                .get("prompt")
                .and_then(Value::as_array)
                .ok_or_else(|| AcpError::JsonRpc("missing prompt".into()))?,
        )?;

        let messages = content_blocks_to_messages(&blocks);
        if messages.is_empty() {
            return Err(AcpError::JsonRpc("empty prompt".into()));
        }

        // 确保默认 lane 存在（`session/new` / `session/load` 已 spawn；此处兜底）。
        self.ensure_default_lane(&session_id).await?;

        // 订阅事件流（在发送 prompt 前订阅，保证不漏 run 事件）。
        let mut rx = self
            .server
            .subscribe(&session_id, DEFAULT_LANE)
            .await
            .ok_or_else(|| AcpError::Server(ServerError::LaneNotFound(DEFAULT_LANE.into())))?;

        // 发送 prompt（入队即返回，run 在后台进行）。
        self.server
            .prompt(&session_id, DEFAULT_LANE, messages)
            .await?;

        // 消费事件流：逐条推送 session/update；跟踪 stopReason；AgentEnd 时结束。
        let mut last_turn_stop: Option<StopReason> = None;
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let Some(update) = map_event_to_update(&event) {
                        let notify_params = json!({ "sessionId": session_id, "update": update });
                        // 通知失败（client 已断开）：立即结束 prompt（中止 lane）并返回
                        // 错误，避免客户端已断开但 prompt 继续消耗 runtime（建议 1）。
                        if let Err(e) = client.notify("session/update", notify_params).await {
                            let _ = self.server.abort(&session_id, DEFAULT_LANE).await;
                            return Err(e);
                        }
                    }
                    if let AgentEvent::TurnEnd { message, .. } = &event
                        && let Some(sr) = &message.stop_reason
                    {
                        last_turn_stop = Some(sr.clone());
                    }
                    if let AgentEvent::AgentEnd { messages } = &event {
                        // 兜底：无 TurnEnd 时取 transcript 末条 assistant 消息的 stop_reason。
                        if last_turn_stop.is_none() {
                            last_turn_stop = messages.iter().rev().find_map(|m| match m.as_ref() {
                                Message::Assistant(a) => a.stop_reason.clone(),
                                _ => None,
                            });
                        }
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }

        let stop_reason = last_turn_stop.unwrap_or(StopReason::Completed);
        Ok(json!({ "stopReason": acp_stop_reason(&stop_reason) }))
    }

    /// `session/cancel`（c→a，notification）：中止默认 lane。
    pub(crate) async fn handle_cancel(&self, params: Value) -> Result<Value, AcpError> {
        let session_id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| AcpError::JsonRpc("missing sessionId".into()))?
            .to_string();
        self.server.abort(&session_id, DEFAULT_LANE).await?;
        Ok(Value::Null)
    }

    /// `session/set_mode`（c→a，notification）：更新 **目标 session** 的权限模式。
    ///
    /// 权限模式为 session 级（按 `sessionId` 隔离）：必须解析并校验 `sessionId`
    /// 存在，只更新该 session 的模式，不影响其他 session（避免权限串改 / 绕过）。
    pub(crate) async fn handle_set_mode(&self, params: Value) -> Result<Value, AcpError> {
        let session_id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| AcpError::JsonRpc("missing sessionId".into()))?
            .to_string();
        let mode_id = params
            .get("modeId")
            .and_then(Value::as_str)
            .ok_or_else(|| AcpError::JsonRpc("missing modeId".into()))?;
        // 校验 session 存在（不存在的 session 不能修改任何权限状态）。
        let sessions = self.server.list_sessions().await;
        if !sessions.contains(&session_id) {
            return Err(AcpError::Server(ServerError::SessionNotFound(session_id)));
        }
        let mode = PermissionMode::from_mode_id(mode_id);
        let mode_handle = self.mode_for(&session_id).await;
        let mut guard = mode_handle.write().await;
        *guard = mode;
        Ok(Value::Null)
    }

    /// 确保默认 lane 存在（不存在则 spawn；已存在忽略）。
    async fn ensure_default_lane(&self, session_id: &str) -> Result<(), AcpError> {
        match self
            .server
            .spawn_lane_from_factory(session_id, DEFAULT_LANE)
            .await
        {
            Ok(()) => Ok(()),
            Err(ServerError::LaneAlreadyExists(_)) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// 解析 `prompt` 的 `ContentBlock[]`（一期仅文本）。
///
/// 对合法但暂不支持的非文本块（`image` / `resource` 等）返回可诊断的
/// 「不支持内容类型」错误，而非让整个请求落入泛化 serde 错误（建议 3）。
fn parse_content_blocks(prompt: &[Value]) -> Result<Vec<ContentBlock>, AcpError> {
    let mut blocks = Vec::with_capacity(prompt.len());
    for (i, b) in prompt.iter().enumerate() {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => {
                let block: ContentBlock = serde_json::from_value(b.clone()).map_err(|e| {
                    AcpError::JsonRpc(format!("invalid text block at prompt[{i}]: {e}"))
                })?;
                blocks.push(block);
            }
            Some(other) => {
                return Err(AcpError::JsonRpc(format!(
                    "unsupported content type at prompt[{i}]: {other} (phase 1 supports text only)"
                )));
            }
            None => {
                return Err(AcpError::JsonRpc(format!(
                    "invalid content block at prompt[{i}]: missing or non-string 'type'"
                )));
            }
        }
    }
    Ok(blocks)
}
