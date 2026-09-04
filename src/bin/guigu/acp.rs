//! ACP 模式（Task 015）：以 ACP agent 身份 serve stdio（供编辑器子进程拉起）。
//!
//! 装配同 REPL（`assemble`），但以 `AcpAgent::new(server).serve_stdio()` 进入
//! （014 定稿），不再读 stdin 交互。stdio 上的 JSON-RPC 2.0。

use guigu::acp::AcpAgent;
use guigu::server::AgentServer;

use super::error::CliError;

/// 跑 ACP stdio 服务：EOF（client 断开）时返回。
pub async fn run_acp(server: AgentServer) -> Result<(), CliError> {
    let agent = AcpAgent::new(server);
    agent.serve_stdio().await?;
    Ok(())
}
