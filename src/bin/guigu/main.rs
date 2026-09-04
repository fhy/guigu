//! guigu CLI（Task 015）：正式命令行入口。
//!
//! 两个模式：**交互式 REPL**（`run`，默认可省略）与 **`acp`**（serve ACP over
//! stdio，供编辑器子进程拉起）。复用 013 `AgentServer` + 007 adapters + 005/006
//! 内置工具 + 009 `JsonlSessionStorage` 装配真实 agent。
//!
//! 模块拆分（单文件 ≤ 400 行约束）：
//! - `cli`：clap 定义（`Cli` / `Command` / `Provider`）
//! - `error`：`CliError` 错误类型
//! - `assemble`：agent 装配（provider / 工具 / server / session 存储）
//! - `repl`：交互式 REPL 循环
//! - `acp`：ACP stdio 服务入口

mod acp;
mod assemble;
mod cli;
mod error;
mod fake;
mod repl;

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Command};
use error::CliError;

/// CLI 入口：解析参数 → dispatch 到 REPL / ACP 模式。
///
/// 返回 `ExitCode`（而非 `Result`）：`main() -> Result<(), E>` 默认经 `Debug`
/// 打印错误，对用户不友好；此处显式经 `Display` 打印到 stderr 并映射退出码。
#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// CLI 主体：解析参数 → dispatch 到 REPL / ACP 模式。
async fn run() -> Result<(), CliError> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Acp) => {
            let assembled = assemble::assemble(&cli)?;
            acp::run_acp(assembled.server).await
        }
        // `run` 显式或省略（默认）都走交互式 REPL。
        Some(Command::Run) | None => {
            let assembled = assemble::assemble(&cli)?;
            let session_id = assemble::setup_session(&assembled, &cli).await?;
            repl::run_repl(assembled.server, &session_id, assemble::DEFAULT_LANE).await
        }
    }
}
