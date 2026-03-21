use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
    },
    Frame, Terminal,
};
use std::io;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A chat message for display.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    pub timestamp: String,
    pub is_tool: bool,
}

/// Actions the TUI can request from the main loop.
pub enum TuiAction {
    /// User submitted input text.
    Submit(String),
    /// User wants to quit.
    Quit,
    /// User entered a slash command.
    SlashCommand(String),
}

// ---------------------------------------------------------------------------
// Application state
// ---------------------------------------------------------------------------

/// TUI application state.
pub struct App {
    /// Chat history.
    messages: Vec<ChatMessage>,
    /// Current input buffer.
    input: String,
    /// Cursor position in input (byte offset).
    cursor_pos: usize,
    /// Scroll offset for chat history.
    scroll_offset: u16,
    /// Total scrollable lines (computed during draw).
    total_lines: u16,
    /// Model name for status bar.
    model: String,
    /// Session ID for status bar.
    session_id: String,
    /// Token count for status bar.
    tokens: u64,
    /// Cost for status bar.
    cost: f64,
    /// Whether the app should quit.
    should_quit: bool,
    /// Whether we are in scroll mode (PageUp/PageDown scroll chat).
    scroll_mode: bool,
    /// Input history.
    input_history: Vec<String>,
    /// Current history index (-1 means "current input, not browsing").
    history_index: i32,
    /// Saved current input when browsing history.
    saved_input: String,
    /// Status message (shown briefly).
    status_message: Option<String>,
}

impl App {
    /// Create a new application state.
    pub fn new(model: &str, session_id: &str) -> Self {
        Self {
            messages: Vec::new(),
            input: String::new(),
            cursor_pos: 0,
            scroll_offset: 0,
            total_lines: 0,
            model: model.to_string(),
            session_id: session_id.to_string(),
            tokens: 0,
            cost: 0.0,
            should_quit: false,
            scroll_mode: false,
            input_history: Vec::new(),
            history_index: -1,
            saved_input: String::new(),
            status_message: None,
        }
    }

    /// Add a message to the chat.
    pub fn add_message(&mut self, role: &str, content: &str, is_tool: bool) {
        let timestamp = chrono_timestamp();
        self.messages.push(ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
            timestamp,
            is_tool,
        });
        // Auto-scroll to bottom when a new message arrives.
        self.scroll_offset = u16::MAX;
    }

    /// Update status bar info.
    pub fn update_status(&mut self, tokens: u64, cost: f64) {
        self.tokens = tokens;
        self.cost = cost;
    }

    /// Set a temporary status message.
    pub fn set_status(&mut self, msg: &str) {
        self.status_message = Some(msg.to_string());
    }

    /// Get the current input and clear it. The input is also pushed onto the
    /// history stack (if non-empty and different from the last entry).
    pub fn take_input(&mut self) -> String {
        let text = self.input.clone();
        if !text.is_empty() {
            if self.input_history.last().map(|s| s.as_str()) != Some(&text) {
                self.input_history.push(text.clone());
            }
        }
        self.input.clear();
        self.cursor_pos = 0;
        self.history_index = -1;
        self.saved_input.clear();
        text
    }

    /// Handle a key event and optionally return an action for the caller.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<TuiAction> {
        // Clear any one-shot status message on the next keypress.
        self.status_message = None;

        match (key.modifiers, key.code) {
            // ----- Quit -----
            (KeyModifiers::CONTROL, KeyCode::Char('c')) | (_, KeyCode::Esc) => {
                self.should_quit = true;
                return Some(TuiAction::Quit);
            }

            // ----- Clear screen -----
            (KeyModifiers::CONTROL, KeyCode::Char('l')) => {
                self.messages.clear();
                self.scroll_offset = 0;
                self.total_lines = 0;
            }

            // ----- Submit -----
            (_, KeyCode::Enter) => {
                let text = self.take_input();
                if text.is_empty() {
                    return None;
                }
                if text.starts_with('/') {
                    return Some(TuiAction::SlashCommand(text));
                }
                return Some(TuiAction::Submit(text));
            }

            // ----- Scrolling chat -----
            (_, KeyCode::PageUp) => {
                self.scroll_mode = true;
                self.scroll_offset = self.scroll_offset.saturating_sub(10);
            }
            (_, KeyCode::PageDown) => {
                self.scroll_offset = self.scroll_offset.saturating_add(10);
                // Clamping happens during draw().
            }

            // ----- Input history -----
            (_, KeyCode::Up) => {
                self.browse_history_back();
            }
            (_, KeyCode::Down) => {
                self.browse_history_forward();
            }

            // ----- Cursor movement -----
            (_, KeyCode::Left) => {
                if self.cursor_pos > 0 {
                    // Move back one character (handle multi-byte).
                    let prev = prev_char_boundary(&self.input, self.cursor_pos);
                    self.cursor_pos = prev;
                }
            }
            (_, KeyCode::Right) => {
                if self.cursor_pos < self.input.len() {
                    let next = next_char_boundary(&self.input, self.cursor_pos);
                    self.cursor_pos = next;
                }
            }
            (_, KeyCode::Home) | (KeyModifiers::CONTROL, KeyCode::Char('a')) => {
                self.cursor_pos = 0;
            }
            (_, KeyCode::End) | (KeyModifiers::CONTROL, KeyCode::Char('e')) => {
                self.cursor_pos = self.input.len();
            }

            // ----- Editing -----
            (_, KeyCode::Backspace) => {
                if self.cursor_pos > 0 {
                    let prev = prev_char_boundary(&self.input, self.cursor_pos);
                    self.input.drain(prev..self.cursor_pos);
                    self.cursor_pos = prev;
                }
            }
            (_, KeyCode::Delete) => {
                if self.cursor_pos < self.input.len() {
                    let next = next_char_boundary(&self.input, self.cursor_pos);
                    self.input.drain(self.cursor_pos..next);
                }
            }

            // ----- Character input -----
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(c)) => {
                self.input.insert(self.cursor_pos, c);
                self.cursor_pos += c.len_utf8();
            }

            _ => {}
        }

        None
    }

    // -- history helpers --

    fn browse_history_back(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        if self.history_index == -1 {
            // Save whatever the user has typed so far.
            self.saved_input = self.input.clone();
            self.history_index = self.input_history.len() as i32 - 1;
        } else if self.history_index > 0 {
            self.history_index -= 1;
        } else {
            return; // already at oldest
        }
        self.input = self.input_history[self.history_index as usize].clone();
        self.cursor_pos = self.input.len();
    }

    fn browse_history_forward(&mut self) {
        if self.history_index == -1 {
            return; // not browsing
        }
        if (self.history_index as usize) < self.input_history.len() - 1 {
            self.history_index += 1;
            self.input = self.input_history[self.history_index as usize].clone();
            self.cursor_pos = self.input.len();
        } else {
            // Return to the saved current input.
            self.history_index = -1;
            self.input = self.saved_input.clone();
            self.cursor_pos = self.input.len();
        }
    }
}

// ---------------------------------------------------------------------------
// Terminal lifecycle
// ---------------------------------------------------------------------------

/// Initialize the terminal for TUI mode.
pub fn init_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

/// Restore terminal to normal mode.
pub fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    terminal::disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// Draw the entire UI.
pub fn draw(f: &mut Frame, app: &App) {
    let size = f.area();

    // Three vertical chunks: chat, status bar, input.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),    // chat area
            Constraint::Length(1), // status bar
            Constraint::Length(3), // input area
        ])
        .split(size);

    draw_chat(f, app, chunks[0]);
    draw_status_bar(f, app, chunks[1]);
    draw_input(f, app, chunks[2]);
}

/// Render the chat history pane.
fn draw_chat(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line<'_>> = Vec::new();

    for msg in &app.messages {
        let styled_lines = render_message(msg, area.width.saturating_sub(2));
        lines.extend(styled_lines);
        // Blank separator line between messages.
        lines.push(Line::from(""));
    }

    let total_lines = lines.len() as u16;

    // Compute the visible height (inside the border).
    let inner_height = area.height.saturating_sub(2); // top + bottom border
    let max_scroll = total_lines.saturating_sub(inner_height);

    // Clamp scroll offset.
    let scroll = app.scroll_offset.min(max_scroll);

    let chat_text = Text::from(lines);

    let chat = Paragraph::new(chat_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Chat ")
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    f.render_widget(chat, area);

    // Scrollbar
    if total_lines > inner_height {
        let mut scrollbar_state = ScrollbarState::new(max_scroll as usize)
            .position(scroll as usize);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("^"))
            .end_symbol(Some("v"));
        // Render the scrollbar in the inner area (inside the border).
        let scrollbar_area = Rect {
            x: area.x + area.width - 1,
            y: area.y + 1,
            width: 1,
            height: inner_height,
        };
        f.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
    }
}

/// Render a single chat message into a vector of styled `Line`s, handling
/// fenced code block detection.
fn render_message<'a>(msg: &'a ChatMessage, _max_width: u16) -> Vec<Line<'a>> {
    let (prefix, prefix_style) = match (msg.role.as_str(), msg.is_tool) {
        (_, true) => (
            "Tool: ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::DIM),
        ),
        ("user", _) => ("You: ", Style::default().fg(Color::Cyan)),
        ("assistant", _) => ("AI: ", Style::default().fg(Color::Green)),
        _ => (
            "",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
    };

    let content_style = if msg.is_tool {
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM)
    } else if msg.role == "system" {
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM)
    } else {
        Style::default()
    };

    let code_style = Style::default()
        .fg(Color::White)
        .bg(Color::Rgb(40, 40, 40));

    let mut result: Vec<Line<'a>> = Vec::new();
    let mut in_code_block = false;
    let mut first_line = true;

    for raw_line in msg.content.lines() {
        // Detect fenced code block boundaries.
        let trimmed = raw_line.trim_start();
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            let fence_style = Style::default().fg(Color::DarkGray);
            if first_line {
                result.push(Line::from(vec![
                    Span::styled(prefix, prefix_style),
                    Span::styled(raw_line.to_string(), fence_style),
                ]));
                first_line = false;
            } else {
                result.push(Line::from(Span::styled(
                    raw_line.to_string(),
                    fence_style,
                )));
            }
            continue;
        }

        let line_style = if in_code_block {
            code_style
        } else {
            content_style
        };

        if first_line {
            result.push(Line::from(vec![
                Span::styled(prefix, prefix_style),
                Span::styled(raw_line.to_string(), line_style),
            ]));
            first_line = false;
        } else {
            result.push(Line::from(Span::styled(raw_line.to_string(), line_style)));
        }
    }

    // If the message was completely empty, still emit the prefix.
    if first_line {
        result.push(Line::from(Span::styled(prefix, prefix_style)));
    }

    // Append a dim timestamp on the last line (right side would be nice but
    // we keep it simple).
    if !msg.timestamp.is_empty() {
        result.push(Line::from(Span::styled(
            format!("  {}", msg.timestamp),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        )));
    }

    result
}

/// Render the status bar.
fn draw_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let status_text = if let Some(ref msg) = app.status_message {
        msg.clone()
    } else {
        format!(
            " {} | Session: {} | Tokens: {} | Cost: ${:.4}",
            app.model, app.session_id, app.tokens, app.cost,
        )
    };

    let status = Paragraph::new(Line::from(vec![Span::styled(
        status_text,
        Style::default()
            .fg(Color::White)
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )]))
    .style(Style::default().bg(Color::DarkGray));

    f.render_widget(status, area);
}

/// Render the input area.
fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    // Build the display text with a "> " prompt.
    let prompt = "> ";
    let display = format!("{}{}", prompt, &app.input);

    let input_widget = Paragraph::new(display.as_str())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Input ")
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });

    f.render_widget(input_widget, area);

    // Place the cursor. The prompt is 2 chars wide ("| " border + "> ").
    // Border left = 1, prompt = 2.
    let cursor_x = area.x + 1 + prompt.len() as u16 + app.cursor_pos as u16;
    let cursor_y = area.y + 1; // inside border
    f.set_cursor_position((cursor_x, cursor_y));
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Return a simple HH:MM:SS timestamp string.
fn chrono_timestamp() -> String {
    // We avoid pulling in the `chrono` crate; use std instead.
    use std::time::SystemTime;
    let dur = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}

/// Find the byte index of the previous character boundary before `pos`.
fn prev_char_boundary(s: &str, pos: usize) -> usize {
    let mut idx = pos.saturating_sub(1);
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Find the byte index of the next character boundary after `pos`.
fn next_char_boundary(s: &str, pos: usize) -> usize {
    let mut idx = pos + 1;
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx.min(s.len())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn make_key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn test_app_creation() {
        let app = App::new("gpt-4", "test-session");
        assert_eq!(app.model, "gpt-4");
        assert_eq!(app.session_id, "test-session");
        assert!(app.messages.is_empty());
        assert!(app.input.is_empty());
    }

    #[test]
    fn test_add_message() {
        let mut app = App::new("m", "s");
        app.add_message("user", "hello", false);
        assert_eq!(app.messages.len(), 1);
        assert_eq!(app.messages[0].role, "user");
        assert_eq!(app.messages[0].content, "hello");
        assert!(!app.messages[0].is_tool);
    }

    #[test]
    fn test_take_input_adds_history() {
        let mut app = App::new("m", "s");
        app.input = "hello".to_string();
        app.cursor_pos = 5;
        let taken = app.take_input();
        assert_eq!(taken, "hello");
        assert!(app.input.is_empty());
        assert_eq!(app.input_history, vec!["hello".to_string()]);
    }

    #[test]
    fn test_take_input_deduplicates() {
        let mut app = App::new("m", "s");
        app.input = "hello".to_string();
        app.take_input();
        app.input = "hello".to_string();
        app.take_input();
        assert_eq!(app.input_history.len(), 1);
    }

    #[test]
    fn test_char_input() {
        let mut app = App::new("m", "s");
        app.handle_key(make_key(KeyCode::Char('h'), KeyModifiers::NONE));
        app.handle_key(make_key(KeyCode::Char('i'), KeyModifiers::NONE));
        assert_eq!(app.input, "hi");
        assert_eq!(app.cursor_pos, 2);
    }

    #[test]
    fn test_backspace() {
        let mut app = App::new("m", "s");
        app.input = "ab".to_string();
        app.cursor_pos = 2;
        app.handle_key(make_key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(app.input, "a");
        assert_eq!(app.cursor_pos, 1);
    }

    #[test]
    fn test_submit() {
        let mut app = App::new("m", "s");
        app.input = "hello".to_string();
        app.cursor_pos = 5;
        let action = app.handle_key(make_key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(action, Some(TuiAction::Submit(s)) if s == "hello"));
    }

    #[test]
    fn test_slash_command() {
        let mut app = App::new("m", "s");
        app.input = "/quit".to_string();
        app.cursor_pos = 5;
        let action = app.handle_key(make_key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(action, Some(TuiAction::SlashCommand(s)) if s == "/quit"));
    }

    #[test]
    fn test_quit_ctrl_c() {
        let mut app = App::new("m", "s");
        let action = app.handle_key(make_key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(matches!(action, Some(TuiAction::Quit)));
        assert!(app.should_quit);
    }

    #[test]
    fn test_cursor_movement() {
        let mut app = App::new("m", "s");
        app.input = "abcd".to_string();
        app.cursor_pos = 4;
        app.handle_key(make_key(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(app.cursor_pos, 0);
        app.handle_key(make_key(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(app.cursor_pos, 4);
        app.handle_key(make_key(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.cursor_pos, 3);
        app.handle_key(make_key(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.cursor_pos, 4);
    }

    #[test]
    fn test_history_browse() {
        let mut app = App::new("m", "s");
        app.input_history = vec!["first".into(), "second".into()];
        app.input = "current".into();
        app.cursor_pos = 7;

        // Up goes to most recent history entry.
        app.handle_key(make_key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input, "second");

        // Up again goes to oldest.
        app.handle_key(make_key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input, "first");

        // Down goes back to "second".
        app.handle_key(make_key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.input, "second");

        // Down again restores saved input.
        app.handle_key(make_key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.input, "current");
    }

    #[test]
    fn test_update_status() {
        let mut app = App::new("m", "s");
        app.update_status(1000, 0.05);
        assert_eq!(app.tokens, 1000);
        assert!((app.cost - 0.05).abs() < f64::EPSILON);
    }

    #[test]
    fn test_set_status() {
        let mut app = App::new("m", "s");
        app.set_status("Processing...");
        assert_eq!(app.status_message.as_deref(), Some("Processing..."));
        // Any key press clears it.
        app.handle_key(make_key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(app.status_message.is_none());
    }

    #[test]
    fn test_clear_screen() {
        let mut app = App::new("m", "s");
        app.add_message("user", "hi", false);
        app.handle_key(make_key(KeyCode::Char('l'), KeyModifiers::CONTROL));
        assert!(app.messages.is_empty());
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn test_delete_key() {
        let mut app = App::new("m", "s");
        app.input = "abc".to_string();
        app.cursor_pos = 1; // cursor after 'a'
        app.handle_key(make_key(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(app.input, "ac");
        assert_eq!(app.cursor_pos, 1);
    }

    #[test]
    fn test_empty_enter_does_nothing() {
        let mut app = App::new("m", "s");
        let action = app.handle_key(make_key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(action.is_none());
    }

    #[test]
    fn test_page_up_down() {
        let mut app = App::new("m", "s");
        app.scroll_offset = 20;
        app.handle_key(make_key(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(app.scroll_offset, 10);
        app.handle_key(make_key(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.scroll_offset, 20);
    }
}
