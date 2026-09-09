//! TUI 渲染（Task 023）：ratatui 垂直三区布局（状态栏 / 对话区 / 输入框）。
//!
//! `render` 是纯渲染函数（读 `TuiState` → 画到 `Frame`），可用 ratatui
//! `TestBackend` 无头验证（断言 buffer 文本），不依赖真实终端。

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::state::{ConvItem, Status, ToolCard, ToolCardStatus, TuiState};

/// 渲染整屏（垂直三区：状态栏 1 行 / 对话区主体 / 输入框 1 行）。
pub fn render(f: &mut Frame, state: &TuiState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // 状态栏
            Constraint::Min(1),    // 对话区
            Constraint::Length(1), // 输入框
        ])
        .split(f.area());

    render_status_bar(f, chunks[0], state);
    render_conversation(f, chunks[1], state);
    render_input(f, chunks[2], state);
}

/// 状态栏：模型名 · lane · usage · 运行状态（●idle/◌running/✗error）+ 错误。
fn render_status_bar(f: &mut Frame, area: Rect, state: &TuiState) {
    let (status_str, status_style) = match state.status {
        Status::Idle => ("● idle", Style::default().fg(Color::Green)),
        Status::Running => ("◌ running", Style::default().fg(Color::Yellow)),
        Status::Error => ("✗ error", Style::default().fg(Color::Red)),
    };
    let usage_str = state
        .usage
        .as_ref()
        .map(|u| format!("{} tok", u.total_tokens))
        .unwrap_or_default();
    let mut spans = vec![
        Span::styled(
            format!(" {} ", state.model),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("· {} · ", state.lane)),
        Span::raw(format!("{usage_str} · ")),
        Span::styled(status_str, status_style),
    ];
    if let Some(err) = &state.error {
        spans.push(Span::styled(
            format!("  {err}"),
            Style::default().fg(Color::Red),
        ));
    }
    let line = Line::from(spans);
    f.render_widget(
        Paragraph::new(line).style(Style::default().bg(Color::DarkGray)),
        area,
    );
}

/// 对话区（可滚动）：user / assistant（流式）/ 工具卡片，按 `state.scroll` 滚动。
fn render_conversation(f: &mut Frame, area: Rect, state: &TuiState) {
    let width = area.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    for item in &state.conv {
        match item {
            ConvItem::User { text } => {
                lines.push(Line::from(Span::styled(
                    "> ",
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::BOLD),
                )));
                if !text.is_empty() {
                    for l in wrap_text(text, width.saturating_sub(2)) {
                        lines.push(Line::from(Span::raw(format!("  {l}"))));
                    }
                }
            }
            ConvItem::Assistant { text } => {
                if !text.is_empty() {
                    for l in wrap_text(text, width) {
                        lines.push(Line::from(Span::raw(l)));
                    }
                }
            }
            ConvItem::Tool(card) => {
                lines.extend(render_tool_card(card, width));
            }
        }
    }
    // 流式气泡：仅思考（无文本）显示「思考中…」；有文本则显示增量。
    if let Some(streaming) = &state.streaming {
        if streaming.text.is_empty() && !streaming.thinking.is_empty() {
            lines.push(Line::from(Span::styled(
                " thinking…",
                Style::default().fg(Color::Gray),
            )));
        }
        if !streaming.text.is_empty() {
            for l in wrap_text(&streaming.text, width) {
                lines.push(Line::from(Span::raw(l)));
            }
        }
    }
    // 滚动：`state.scroll` = 距底部行数（0 = 贴底）。
    let total = lines.len();
    let visible = area.height as usize;
    let max_scroll = total.saturating_sub(visible);
    let scroll = state.scroll.min(max_scroll);
    let start = total.saturating_sub(visible).saturating_sub(scroll);
    let end = total.saturating_sub(scroll);
    let visible_lines: Vec<Line> = lines[start..end].to_vec();
    f.render_widget(Paragraph::new(visible_lines), area);
}

/// 工具卡片（内联）：标题行（tool: 名 + 状态）+ 参数行 + 输出行 + 收尾。
fn render_tool_card(card: &ToolCard, width: usize) -> Vec<Line<'_>> {
    let (status_str, status_color) = match card.status {
        ToolCardStatus::Running | ToolCardStatus::RunningPartial => ("running", Color::Yellow),
        ToolCardStatus::Done => ("done", Color::Green),
        ToolCardStatus::Failed => ("failed", Color::Red),
    };
    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("┌─ tool: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            card.name.clone(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" ─ {status_str}"),
            Style::default().fg(status_color),
        ),
    ]));
    if !card.args.is_empty() {
        let args = truncate_display(&card.args, width.saturating_sub(6));
        lines.push(Line::from(Span::styled(
            format!("│ $ {args}"),
            Style::default().fg(Color::Gray),
        )));
    }
    if !card.output.is_empty() {
        for l in wrap_text(&card.output, width.saturating_sub(2)) {
            lines.push(Line::from(Span::raw(format!("│ {l}"))));
        }
    }
    lines.push(Line::from(Span::styled(
        "└─",
        Style::default().fg(Color::DarkGray),
    )));
    lines
}

/// 输入框（单行 + 历史，Enter 提交）。
fn render_input(f: &mut Frame, area: Rect, state: &TuiState) {
    let line = Line::from(vec![
        Span::styled(
            "> ",
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(state.input.clone()),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// 按 `width`（char）简单折行（处理 `\n`，不做词边界）。
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch == '\n' {
            lines.push(std::mem::take(&mut current));
            continue;
        }
        if current.chars().count() >= width {
            lines.push(std::mem::take(&mut current));
        }
        current.push(ch);
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

/// 截断显示文本（按 char，超出追加省略号）。
fn truncate_display(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else {
        let head: String = chars[..max].iter().collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// 渲染到 `TestBackend` 并返回 buffer 文本（逐行拼接）。
    fn render_to_buffer(state: &TuiState, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("new terminal");
        terminal.draw(|f| render(f, state)).expect("draw");
        let buf = terminal.backend().buffer();
        let area = buf.area();
        let w = area.width as usize;
        let mut lines = Vec::new();
        for y in 0..area.height as usize {
            let mut line = String::new();
            for x in 0..w {
                line.push_str(buf.content()[y * w + x].symbol());
            }
            lines.push(line);
        }
        lines.join("\n")
    }

    #[test]
    fn status_bar_shows_model_and_status() {
        let mut state = TuiState::new("test-model".into(), "default".into());
        state.status = Status::Running;
        let buf = render_to_buffer(&state, 80, 24);
        assert!(
            buf.contains("test-model"),
            "status bar should show model, got:\n{buf}"
        );
        assert!(
            buf.contains("running"),
            "status bar should show running, got:\n{buf}"
        );
    }

    #[test]
    fn status_bar_shows_idle() {
        let state = TuiState::new("m".into(), "l".into());
        let buf = render_to_buffer(&state, 80, 24);
        assert!(buf.contains("idle"), "should show idle, got:\n{buf}");
    }

    #[test]
    fn conversation_shows_user_and_assistant_bubbles() {
        let mut state = TuiState::new("m".into(), "l".into());
        state.conv.push(ConvItem::User {
            text: "hello".into(),
        });
        state.conv.push(ConvItem::Assistant {
            text: "world".into(),
        });
        let buf = render_to_buffer(&state, 80, 24);
        assert!(buf.contains("hello"), "user bubble, got:\n{buf}");
        assert!(buf.contains("world"), "assistant bubble, got:\n{buf}");
    }

    #[test]
    fn tool_card_shows_name_and_status() {
        let mut state = TuiState::new("m".into(), "l".into());
        state.conv.push(ConvItem::Tool(ToolCard {
            id: "t1".into(),
            name: "bash".into(),
            args: "{\"cmd\":\"ls\"}".into(),
            status: ToolCardStatus::Done,
            output: "file.txt".into(),
        }));
        let buf = render_to_buffer(&state, 80, 24);
        assert!(buf.contains("bash"), "tool name, got:\n{buf}");
        assert!(buf.contains("done"), "tool status, got:\n{buf}");
        assert!(buf.contains("file.txt"), "tool output, got:\n{buf}");
    }

    #[test]
    fn input_box_shows_buffer() {
        let mut state = TuiState::new("m".into(), "l".into());
        state.input = "typing here".into();
        let buf = render_to_buffer(&state, 80, 24);
        assert!(buf.contains("typing here"), "input box, got:\n{buf}");
    }

    #[test]
    fn streaming_thinking_shows_indicator() {
        let mut state = TuiState::new("m".into(), "l".into());
        state.streaming = Some(super::super::state::StreamingBubble {
            text: String::new(),
            thinking: "hmm".into(),
        });
        let buf = render_to_buffer(&state, 80, 24);
        assert!(buf.contains("thinking"), "thinking indicator, got:\n{buf}");
    }

    #[test]
    fn wrap_text_breaks_long_lines() {
        let lines = wrap_text("abcdefghij", 4);
        assert_eq!(lines, vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn wrap_text_handles_newline() {
        let lines = wrap_text("ab\ncd", 4);
        assert_eq!(lines, vec!["ab", "cd"]);
    }
}
