//! 交互式 REPL（Task 015）：行式 stdin 读 → prompt → 事件流打印。
//!
//! 不引入行编辑库（rustyline/dialoguer），保持 minimal：`tokio::io::stdin` 逐行读。
//! 空行忽略；`/quit` / `/exit` / Ctrl-D（EOF）退出；退出前 `server.shutdown`。
//! 单次 prompt 错误打印但不退出（继续读下一行）。

use std::io::Write;

use guigu::core::event::AgentEvent;
use guigu::core::message::{Message, ToolResultContent, UserContent, UserMessage};
use guigu::core::provider::AssistantEvent;
use guigu::server::{AgentServer, ServerError};
use tokio::io::AsyncBufReadExt;
use tokio::sync::broadcast;

use super::error::CliError;

/// 跑交互式 REPL：读行 → prompt → 消费事件流（text/tool 进度）→ 直到退出。
pub async fn run_repl(
    server: AgentServer,
    session_id: &str,
    lane_id: &str,
) -> Result<(), CliError> {
    // 订阅事件流（在发 prompt 前订阅，保证不漏 run 事件）。
    let mut rx = server
        .subscribe(session_id, lane_id)
        .await
        .ok_or_else(|| CliError::Server(ServerError::LaneNotFound(lane_id.to_string())))?;

    let mut stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let mut buf = String::new();

    println!("guigu REPL (session: {session_id}). Type /quit or Ctrl-D to exit.");

    loop {
        buf.clear();
        print!("> ");
        std::io::stdout().flush()?;

        match stdin.read_line(&mut buf).await {
            Ok(0) => break, // EOF（Ctrl-D）
            Ok(_) => {
                let line = buf.trim();
                if line.is_empty() {
                    continue;
                }
                if line == "/quit" || line == "/exit" {
                    break;
                }
                let msg = Message::User(UserMessage {
                    content: vec![UserContent::Text {
                        text: line.to_string(),
                    }],
                    timestamp: 0,
                });
                if let Err(e) = server.prompt(session_id, lane_id, vec![msg]).await {
                    eprintln!("prompt error: {e}");
                    continue;
                }
                consume_events(&mut rx).await;
            }
            Err(e) => return Err(CliError::Io(e)),
        }
    }

    // 退出前 shutdown（等 runtime task 退出，桥接 task 随事件流关闭退出）。
    server.shutdown().await?;
    Ok(())
}

/// 消费事件流直到 `AgentEnd`：text/tool 进度打印到 stdout/stderr。
async fn consume_events(rx: &mut broadcast::Receiver<AgentEvent>) {
    loop {
        match rx.recv().await {
            Ok(event) => {
                print_event(&event);
                if matches!(event, AgentEvent::AgentEnd { .. }) {
                    break;
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                eprintln!("\x1b[90m(lagged, skipped {n} events)\x1b[0m");
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
    println!();
}

/// 打印一条事件（text 增量 → stdout；tool 进度 → stderr；其余忽略）。
fn print_event(event: &AgentEvent) {
    match event {
        AgentEvent::MessageUpdate {
            assistant_event, ..
        } => match assistant_event {
            AssistantEvent::TextDelta { text } => {
                print!("{text}");
                let _ = std::io::stdout().flush();
            }
            AssistantEvent::ThinkingDelta { thinking } => {
                print!("\x1b[90m{thinking}\x1b[0m");
                let _ = std::io::stdout().flush();
            }
            _ => {}
        },
        AgentEvent::ToolExecutionStart {
            tool_name, args, ..
        } => {
            eprintln!("\x1b[36m[tool] {tool_name} {args}\x1b[0m");
        }
        AgentEvent::ToolExecutionEnd {
            tool_name,
            result,
            is_error,
            ..
        } => {
            let status = if *is_error { "failed" } else { "ok" };
            let text = result
                .content
                .iter()
                .find_map(|c| match c {
                    ToolResultContent::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .unwrap_or("");
            let preview: String = text.chars().take(200).collect();
            eprintln!("\x1b[36m[tool] {tool_name} {status}: {preview}\x1b[0m");
        }
        _ => {}
    }
}
