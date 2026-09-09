//! TUI 输入处理（Task 023）：`handle_key` 纯逻辑（编辑 buffer / 提交 / 中止 / 退出）。
//!
//! `Key` 是简化键位枚举（不依赖 crossterm），使 `handle_key` 可无终端单测；
//! crossterm `KeyEvent` → `Key` 的转换在 `mod.rs`（feature 门控）。

use super::state::{Status, TuiState};

/// 简化键位（纯逻辑 + 可测，不依赖 crossterm）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// 可打印字符。
    Char(char),
    /// 回车（提交）。
    Enter,
    /// 退格。
    Backspace,
    /// 上箭头（历史向前）。
    Up,
    /// 下箭头（历史向后）。
    Down,
    /// 上翻页（滚动）。
    PageUp,
    /// 下翻页（滚动）。
    PageDown,
    /// Ctrl-C（中止 / 退出）。
    CtrlC,
    /// Esc（退出）。
    Esc,
    /// 其它（忽略）。
    Other,
}

/// 键位处理结果（驱动事件循环的副作用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    /// 无副作用（仅更新状态，重绘）。
    None,
    /// 提交 prompt。
    Submit(String),
    /// 中止当前 run（`server.abort`）。
    Abort,
    /// 退出 TUI。
    Exit,
}

/// 每页滚动行数。
const PAGE_LINES: usize = 10;

/// 处理一个键位（纯逻辑，无 IO）。
///
/// 键位语义：`Enter` 提交（`/quit`/`/exit` 退出）、`↑/↓` 历史、`Ctrl-C`
/// 首次中止当前 run（运行中）/ 退出（空闲）、再次 `Ctrl-C` 或 `Esc` 退出、
/// `PageUp/PageDown` 滚动。
pub fn handle_key(state: &mut TuiState, key: Key) -> KeyAction {
    match key {
        Key::Enter => {
            let input = state.input.trim().to_string();
            if input.is_empty() {
                return KeyAction::None;
            }
            if input == "/quit" || input == "/exit" {
                state.input.clear();
                return KeyAction::Exit;
            }
            state.history.push(input.clone());
            state.history_cursor = usize::MAX;
            state.input.clear();
            KeyAction::Submit(input)
        }
        Key::Char(c) => {
            // 浏览历史时输入新字符：丢弃历史内容，从空开始。
            if state.history_cursor != usize::MAX {
                state.input.clear();
                state.history_cursor = usize::MAX;
            }
            state.input.push(c);
            KeyAction::None
        }
        Key::Backspace => {
            state.input.pop();
            KeyAction::None
        }
        Key::Up => {
            if state.history.is_empty() {
                return KeyAction::None;
            }
            if state.history_cursor == usize::MAX {
                state.history_cursor = state.history.len() - 1;
            } else if state.history_cursor > 0 {
                state.history_cursor -= 1;
            }
            state.input = state.history[state.history_cursor].clone();
            KeyAction::None
        }
        Key::Down => {
            if state.history_cursor == usize::MAX {
                return KeyAction::None;
            }
            if state.history_cursor + 1 < state.history.len() {
                state.history_cursor += 1;
                state.input = state.history[state.history_cursor].clone();
            } else {
                // 越过最后一条历史：回到空输入。
                state.history_cursor = usize::MAX;
                state.input.clear();
            }
            KeyAction::None
        }
        Key::CtrlC => match state.status {
            Status::Running => {
                if state.abort_requested {
                    KeyAction::Exit
                } else {
                    state.abort_requested = true;
                    KeyAction::Abort
                }
            }
            _ => KeyAction::Exit,
        },
        Key::Esc => KeyAction::Exit,
        Key::PageUp => {
            state.scroll = state.scroll.saturating_add(PAGE_LINES);
            KeyAction::None
        }
        Key::PageDown => {
            state.scroll = state.scroll.saturating_sub(PAGE_LINES);
            KeyAction::None
        }
        Key::Other => KeyAction::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_history() -> TuiState {
        let mut s = TuiState::new("m".into(), "l".into());
        s.history = vec!["first".into(), "second".into()];
        s
    }

    #[test]
    fn enter_submits_and_pushes_history() {
        let mut s = TuiState::new("m".into(), "l".into());
        s.input = "hello".into();
        assert_eq!(
            handle_key(&mut s, Key::Enter),
            KeyAction::Submit("hello".into())
        );
        assert_eq!(s.input, "");
        assert_eq!(s.history, vec!["hello".to_string()]);
    }

    #[test]
    fn enter_empty_is_noop() {
        let mut s = TuiState::new("m".into(), "l".into());
        s.input = "   ".into();
        assert_eq!(handle_key(&mut s, Key::Enter), KeyAction::None);
        assert!(s.history.is_empty());
    }

    #[test]
    fn enter_quit_exits() {
        let mut s = TuiState::new("m".into(), "l".into());
        s.input = "/quit".into();
        assert_eq!(handle_key(&mut s, Key::Enter), KeyAction::Exit);
    }

    #[test]
    fn char_appends_to_input() {
        let mut s = TuiState::new("m".into(), "l".into());
        handle_key(&mut s, Key::Char('a'));
        handle_key(&mut s, Key::Char('b'));
        assert_eq!(s.input, "ab");
    }

    #[test]
    fn backspace_pops() {
        let mut s = TuiState::new("m".into(), "l".into());
        s.input = "ab".into();
        handle_key(&mut s, Key::Backspace);
        assert_eq!(s.input, "a");
    }

    #[test]
    fn up_down_navigate_history() {
        let mut s = state_with_history();
        // 空输入 → Up 取最近一条（"second"）。
        assert_eq!(handle_key(&mut s, Key::Up), KeyAction::None);
        assert_eq!(s.input, "second");
        // 再 Up → "first"。
        handle_key(&mut s, Key::Up);
        assert_eq!(s.input, "first");
        // 再 Up 不越界。
        handle_key(&mut s, Key::Up);
        assert_eq!(s.input, "first");
        // Down → "second"。
        handle_key(&mut s, Key::Down);
        assert_eq!(s.input, "second");
        // 再 Down → 越过末尾，清空。
        handle_key(&mut s, Key::Down);
        assert_eq!(s.input, "");
        assert_eq!(s.history_cursor, usize::MAX);
    }

    #[test]
    fn typing_after_history_discards_history() {
        let mut s = state_with_history();
        handle_key(&mut s, Key::Up); // input = "second"
        handle_key(&mut s, Key::Char('x'));
        assert_eq!(s.input, "x");
        assert_eq!(s.history_cursor, usize::MAX);
    }

    #[test]
    fn ctrl_c_running_aborts_then_exits() {
        let mut s = TuiState::new("m".into(), "l".into());
        s.status = Status::Running;
        assert_eq!(handle_key(&mut s, Key::CtrlC), KeyAction::Abort);
        assert!(s.abort_requested);
        // 再次 Ctrl-C（仍运行中）→ 退出。
        assert_eq!(handle_key(&mut s, Key::CtrlC), KeyAction::Exit);
    }

    #[test]
    fn ctrl_c_idle_exits() {
        let mut s = TuiState::new("m".into(), "l".into());
        s.status = Status::Idle;
        assert_eq!(handle_key(&mut s, Key::CtrlC), KeyAction::Exit);
    }

    #[test]
    fn esc_exits() {
        let mut s = TuiState::new("m".into(), "l".into());
        assert_eq!(handle_key(&mut s, Key::Esc), KeyAction::Exit);
    }

    #[test]
    fn page_up_down_scroll() {
        let mut s = TuiState::new("m".into(), "l".into());
        handle_key(&mut s, Key::PageUp);
        assert_eq!(s.scroll, PAGE_LINES);
        handle_key(&mut s, Key::PageUp);
        assert_eq!(s.scroll, PAGE_LINES * 2);
        handle_key(&mut s, Key::PageDown);
        assert_eq!(s.scroll, PAGE_LINES);
        // 不越下界。
        for _ in 0..5 {
            handle_key(&mut s, Key::PageDown);
        }
        assert_eq!(s.scroll, 0);
    }
}
