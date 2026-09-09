//! TUI 模式（Task 023）：ratatui 全屏终端 UI。
//!
//! TUI 是**纯渲染层**：订阅事件流 + 读快照，不持有 agent 状态，不改 001/003
//! 的「单写者」契约。复用 022 模型配置 + 013 `AgentServer` + 015 装配逻辑
//! （`assemble.rs`）。
//!
//! 模块拆分（单文件 ≤ 400 行约束）：
//! - `state`：`TuiState` + `apply_event`（事件 → 状态纯映射，可单测）
//! - `input`：`Key` + `handle_key`（输入处理纯逻辑，可单测）
//! - `render`：`render`（ratatui 渲染 + `TestBackend` 无头测试）
//! - 本文件：`run`（事件循环 + 终端生命周期）

mod input;
mod render;
mod state;

#[cfg(test)]
#[path = "loop_tests.rs"]
mod loop_tests;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyEvent};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::{broadcast, mpsc};

use guigu::core::event::AgentEvent;
use guigu::core::message::{Message, UserContent, UserMessage};
use guigu::server::{AgentServer, ServerError};

use super::error::CliError;
use input::{Key, KeyAction, handle_key};
use render::render;
use state::{Status, TuiState, apply_event};

/// 键盘 reader 输出（键事件或读错误）。
enum ReaderEvent {
    /// 键事件。
    Key(KeyEvent),
    /// 读错误（终端断开等）——回送主循环，区分终端断开与正常退出。
    Error(String),
}

/// UI 循环 → 命令 task 的命令（prompt/abort 在独立 task 执行，不阻塞 UI）。
enum TuiCommand {
    /// 提交 prompt（失败经结果 channel 回送 UI 循环）。
    Prompt { messages: Vec<Message> },
    /// 中止当前 run。
    Abort,
}

/// 命令 task 结果（回送 UI 循环）。
enum CommandResult {
    /// prompt 提交成功（UI 循环无需处理——事件回环由订阅驱动）。
    PromptAccepted,
    /// prompt 提交失败。
    PromptFailed(String),
}

/// 跑 TUI：终端 setup → 事件循环（键盘 / agent 事件 / 命令结果 / 100ms tick）→ 终端恢复。
///
/// raw mode 失败（无 TTY）返回清晰错误（不 panic）；**所有初始化失败路径**（订阅
/// 失败 / 终端 setup 任一步失败）都恢复已完成步骤的终端状态并 `server.shutdown`
/// （不遗留 runtime task）；退出前 `server.shutdown`，终端状态（raw mode /
/// alt screen / 光标）异常时也恢复。
pub async fn run(
    server: AgentServer,
    session_id: &str,
    lane_id: &str,
    model: &str,
) -> Result<(), CliError> {
    // 订阅事件流（在发 prompt 前订阅，保证不漏 run 事件）。
    let mut rx = match server.subscribe(session_id, lane_id).await {
        Some(rx) => rx,
        None => {
            shutdown_quietly(&server).await;
            return Err(CliError::Server(ServerError::LaneNotFound(
                lane_id.to_string(),
            )));
        }
    };

    let mut tui_state = TuiState::new(model.to_string(), lane_id.to_string());

    // 终端 setup：任一步失败 → `setup_terminal` 已恢复已完成步骤的终端状态，
    // 此处 shutdown 避免遗留 runtime task，再返回原始错误。
    let mut terminal = match setup_terminal() {
        Ok(terminal) => terminal,
        Err(e) => {
            shutdown_quietly(&server).await;
            return Err(e);
        }
    };

    // 键盘 reader 线程：crossterm poll 读 → channel（供 select 异步化）。
    // 用 stop 标志 + poll 超时（而非阻塞 `event::read()`）：主任务退出时置位
    // stop，线程在下一个 poll 周期退出——阻塞 read 不会因 `disable_raw_mode`
    // 解除，直接 `reader.join()` 会在真实 TTY 下挂起（进程无法退出）。
    // 读错误（终端断开等）经 `ReaderEvent::Error` 回送主循环（区分正常退出）。
    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<ReaderEvent>();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let reader = std::thread::spawn(move || {
        while !stop_clone.load(Ordering::Relaxed) {
            match event::poll(Duration::from_millis(50)) {
                Ok(true) => match event::read() {
                    Ok(Event::Key(key)) => {
                        if key_tx.send(ReaderEvent::Key(key)).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {} // 非键事件（鼠标/焦点等）忽略。
                    Err(e) => {
                        let _ =
                            key_tx.send(ReaderEvent::Error(format!("terminal read failed: {e}")));
                        break;
                    }
                },
                Ok(false) => continue,
                Err(e) => {
                    let _ = key_tx.send(ReaderEvent::Error(format!("terminal poll failed: {e}")));
                    break;
                }
            }
        }
    });

    // 命令 task：prompt/abort 在独立 task 执行（不阻塞 UI 事件循环）；
    // prompt 失败经结果 channel 回送 UI 循环。
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<TuiCommand>();
    let (result_tx, mut result_rx) = mpsc::unbounded_channel::<CommandResult>();
    let cmd_task = spawn_command_task(server.clone(), session_id, lane_id, cmd_rx, result_tx);

    let result = run_loop(
        &mut |state: &TuiState| {
            terminal
                .draw(|f| render(f, state))
                .map(|_| ())
                .map_err(|e| CliError::Tui(format!("terminal draw failed: {e}")))
        },
        &mut tui_state,
        &mut rx,
        &mut key_rx,
        &mut result_rx,
        &cmd_tx,
    )
    .await;

    // 终端恢复（总是执行，即使出错）。
    restore_terminal();

    // 停止键盘 reader 线程（置位 stop，线程在下一 poll 周期退出），再 join。
    stop.store(true, Ordering::Relaxed);
    let _ = reader.join();

    // 等命令 task 退出（shutdown 前无 in-flight prompt/abort）。
    drop(cmd_tx);
    if let Err(e) = cmd_task.await {
        eprintln!("warning: tui command task panicked: {e}");
    }

    // 退出前 shutdown（等 runtime task 退出，桥接 task 随事件流关闭退出）。
    server.shutdown().await?;

    result
}

/// 终端 setup：raw mode + alternate screen + 隐藏光标 + `Terminal`。
///
/// 任一步失败 → 恢复已完成步骤的终端状态（disable raw mode / 离开 alt screen /
/// 显示光标）并返回原始错误；调用方负责 `server.shutdown`。
fn setup_terminal() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>, CliError> {
    enable_raw_mode().map_err(|e| {
        CliError::Tui(format!(
            "TUI requires an interactive terminal (TTY); raw mode failed: {e}"
        ))
    })?;
    let mut stdout = std::io::stdout();
    if let Err(e) = execute!(stdout, EnterAlternateScreen, Hide) {
        disable_raw_mode().ok();
        return Err(CliError::Tui(format!(
            "failed to enter alternate screen: {e}"
        )));
    }
    let backend = CrosstermBackend::new(stdout);
    match Terminal::new(backend) {
        Ok(terminal) => Ok(terminal),
        Err(e) => {
            disable_raw_mode().ok();
            execute!(std::io::stdout(), LeaveAlternateScreen, Show).ok();
            Err(CliError::Tui(format!("failed to init terminal: {e}")))
        }
    }
}

/// 终端恢复（退出时总是执行，即使出错）：disable raw mode + 离开 alt screen + 显示光标。
fn restore_terminal() {
    disable_raw_mode().ok();
    execute!(std::io::stdout(), LeaveAlternateScreen, Show).ok();
}

/// 失败路径 shutdown：shutdown 自身失败记 stderr（不覆盖正在返回的原始错误）。
async fn shutdown_quietly(server: &AgentServer) {
    if let Err(e) = server.shutdown().await {
        eprintln!("warning: server shutdown failed: {e}");
    }
}

/// 启动命令 task：串行执行 prompt/abort（独立 task，不阻塞 UI 事件循环）。
///
/// prompt 失败经 `result_tx` 回送 UI 循环（`PromptFailed`）；成功发
/// `PromptAccepted`（UI 循环无需处理——事件回环由订阅驱动）。
fn spawn_command_task(
    server: AgentServer,
    session_id: &str,
    lane_id: &str,
    mut cmd_rx: mpsc::UnboundedReceiver<TuiCommand>,
    result_tx: mpsc::UnboundedSender<CommandResult>,
) -> tokio::task::JoinHandle<()> {
    let session_id = session_id.to_string();
    let lane_id = lane_id.to_string();
    tokio::spawn(async move {
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                TuiCommand::Prompt { messages } => {
                    let result = server
                        .prompt(&session_id, &lane_id, messages)
                        .await
                        .map_err(|e| e.to_string());
                    let _ = result_tx.send(match result {
                        Ok(()) => CommandResult::PromptAccepted,
                        Err(e) => CommandResult::PromptFailed(e),
                    });
                }
                TuiCommand::Abort => {
                    let _ = server.abort(&session_id, &lane_id).await;
                }
            }
        }
    })
}

/// 事件循环：`tokio::select!`（键盘 / agent 事件 / 命令结果 / 100ms tick）→ 每次循环后重绘。
///
/// `draw` 是重绘回调（生产：`terminal.draw(render)`；测试：计数/无头），使循环
/// 逻辑可无真实终端测试。prompt 提交经命令 task（独立 task，不阻塞 UI 事件循环）；
/// 事件回环由订阅驱动。`Ctrl-C` → `server.abort`（不直接杀进程）；退出前由 `run`
/// 做 `server.shutdown`。draw 错误转 `CliError::Tui` 退出循环（外层统一恢复终端
/// 并 shutdown）；终端读错误（断开等）同样退出并保留错误信息。
async fn run_loop(
    draw: &mut impl FnMut(&TuiState) -> Result<(), CliError>,
    state: &mut TuiState,
    rx: &mut broadcast::Receiver<AgentEvent>,
    key_rx: &mut mpsc::UnboundedReceiver<ReaderEvent>,
    result_rx: &mut mpsc::UnboundedReceiver<CommandResult>,
    cmd_tx: &mpsc::UnboundedSender<TuiCommand>,
) -> Result<(), CliError> {
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            maybe_input = key_rx.recv() => {
                match maybe_input {
                    Some(ReaderEvent::Key(key_event)) => {
                        let key = from_crossterm(&key_event);
                        let action = handle_key(state, key);
                        match action {
                            KeyAction::None => {}
                            KeyAction::Submit(prompt) => {
                                // 提交经命令 task（独立 task，不阻塞 UI 事件循环）。
                                let msg = Message::User(UserMessage {
                                    content: vec![UserContent::Text { text: prompt }],
                                    timestamp: 0,
                                });
                                if cmd_tx
                                    .send(TuiCommand::Prompt {
                                        messages: vec![msg],
                                    })
                                    .is_err()
                                {
                                    state.error = Some("command task exited".to_string());
                                    state.status = Status::Error;
                                }
                            }
                            KeyAction::Abort => {
                                let _ = cmd_tx.send(TuiCommand::Abort);
                            }
                            KeyAction::Exit => return Ok(()),
                        }
                    }
                    // 终端读失败（断开等）：上报 UI 并退出循环（外层统一恢复终端 + shutdown）。
                    Some(ReaderEvent::Error(reason)) => {
                        state.error = Some(reason.clone());
                        state.status = Status::Error;
                        return Err(CliError::Tui(reason));
                    }
                    // reader 线程退出（channel 关闭）。
                    None => return Ok(()),
                }
            }
            maybe_event = rx.recv() => match maybe_event {
                Ok(event) => apply_event(state, &event),
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
            maybe_result = result_rx.recv() => match maybe_result {
                // prompt 提交失败（命令 task 回送）。
                Some(CommandResult::PromptFailed(err)) => {
                    state.error = Some(err);
                    state.status = Status::Error;
                }
                // prompt 提交成功（事件回环由订阅驱动，无需处理）或命令 task 退出。
                Some(CommandResult::PromptAccepted) | None => {}
            },
            _ = ticker.tick() => {
                // 非事件类刷新（光标闪烁等）。
            }
        }
        draw(state)?;
    }
}

/// crossterm `KeyEvent` → 简化 `Key`（feature 门控，用 crossterm）。
fn from_crossterm(event: &KeyEvent) -> Key {
    use crossterm::event::{KeyCode, KeyModifiers};
    if event.modifiers.contains(KeyModifiers::CONTROL) {
        return match event.code {
            KeyCode::Char('c') => Key::CtrlC,
            _ => Key::Other,
        };
    }
    match event.code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Esc => Key::Esc,
        _ => Key::Other,
    }
}
