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

/// 跑 TUI：终端 setup → 事件循环（键盘 / agent 事件 / 100ms tick）→ 终端恢复。
///
/// raw mode 失败（无 TTY）返回清晰错误（不 panic）；退出前 `server.shutdown`，
/// 终端状态（raw mode / alt screen / 光标）异常时也恢复。
pub async fn run(
    server: AgentServer,
    session_id: &str,
    lane_id: &str,
    model: &str,
) -> Result<(), CliError> {
    // 订阅事件流（在发 prompt 前订阅，保证不漏 run 事件）。
    let mut rx = server
        .subscribe(session_id, lane_id)
        .await
        .ok_or_else(|| CliError::Server(ServerError::LaneNotFound(lane_id.to_string())))?;

    let mut tui_state = TuiState::new(model.to_string(), lane_id.to_string());

    // 终端 setup：raw mode + alternate screen + 隐藏光标。
    // raw mode 失败（无 TTY）→ 清晰错误（不 panic）。
    enable_raw_mode().map_err(|e| {
        CliError::Tui(format!(
            "TUI requires an interactive terminal (TTY); raw mode failed: {e}"
        ))
    })?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, Hide).map_err(|e| {
        disable_raw_mode().ok();
        CliError::Tui(format!("failed to enter alternate screen: {e}"))
    })?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(|e| {
        disable_raw_mode().ok();
        execute!(std::io::stdout(), LeaveAlternateScreen, Show).ok();
        CliError::Tui(format!("failed to init terminal: {e}"))
    })?;

    // 键盘 reader 线程：crossterm poll 读 → channel（供 select 异步化）。
    // 用 stop 标志 + poll 超时（而非阻塞 `event::read()`）：主任务退出时置位
    // stop，线程在下一个 poll 周期退出——阻塞 read 不会因 `disable_raw_mode`
    // 解除，直接 `reader.join()` 会在真实 TTY 下挂起（进程无法退出）。
    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<KeyEvent>();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let reader = std::thread::spawn(move || {
        while !stop_clone.load(Ordering::Relaxed) {
            match event::poll(Duration::from_millis(50)) {
                Ok(true) => {
                    if let Ok(Event::Key(key)) = event::read()
                        && key_tx.send(key).is_err()
                    {
                        break;
                    }
                }
                Ok(false) => continue,
                Err(_) => break,
            }
        }
    });

    let result = run_loop(
        &mut terminal,
        &mut tui_state,
        &server,
        session_id,
        lane_id,
        &mut rx,
        &mut key_rx,
    )
    .await;

    // 终端恢复（总是执行，即使出错）。
    disable_raw_mode().ok();
    execute!(std::io::stdout(), LeaveAlternateScreen, Show).ok();

    // 停止键盘 reader 线程（置位 stop，线程在下一 poll 周期退出），再 join。
    stop.store(true, Ordering::Relaxed);
    let _ = reader.join();

    // 退出前 shutdown（等 runtime task 退出，桥接 task 随事件流关闭退出）。
    server.shutdown().await?;

    result
}

/// 事件循环：`tokio::select!`（键盘 / agent 事件 / 100ms tick）→ 每次循环后重绘。
///
/// 提交 prompt 经 `server.prompt`（仅入队命令，不阻塞 UI）；事件回环由订阅驱动。
/// `Ctrl-C` → `server.abort`（不直接杀进程）；退出前由 `run` 做 `server.shutdown`。
async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    state: &mut TuiState,
    server: &AgentServer,
    session_id: &str,
    lane_id: &str,
    rx: &mut broadcast::Receiver<AgentEvent>,
    key_rx: &mut mpsc::UnboundedReceiver<KeyEvent>,
) -> Result<(), CliError> {
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            maybe_key = key_rx.recv() => {
                match maybe_key {
                    Some(key_event) => {
                        let key = from_crossterm(&key_event);
                        let action = handle_key(state, key);
                        match action {
                            KeyAction::None => {}
                            KeyAction::Submit(prompt) => {
                                let msg = Message::User(UserMessage {
                                    content: vec![UserContent::Text { text: prompt }],
                                    timestamp: 0,
                                });
                                if let Err(e) =
                                    server.prompt(session_id, lane_id, vec![msg]).await
                                {
                                    state.error = Some(e.to_string());
                                    state.status = Status::Error;
                                }
                            }
                            KeyAction::Abort => {
                                let _ = server.abort(session_id, lane_id).await;
                            }
                            KeyAction::Exit => return Ok(()),
                        }
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
            _ = ticker.tick() => {
                // 非事件类刷新（光标闪烁等）。
            }
        }
        terminal.draw(|f| render(f, state)).ok();
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
