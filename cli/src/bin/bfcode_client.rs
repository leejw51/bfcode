//! TUI client for connecting to a bfcode gateway server.
//!
//! Usage:
//!   cargo run --bin bfcode_client -- --url http://127.0.0.1:8642
//!   cargo run --bin bfcode_client -- -u http://myserver:8642 -k myapikey

use anyhow::{Context, Result, bail};
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers, poll},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use serde::{Deserialize, Serialize};
use std::io;
use std::time::Duration;

// ---------------------------------------------------------------------------
// CLI arguments
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "bfcode-client", about = "TUI client for bfcode gateway")]
struct Args {
    /// Gateway URL
    #[arg(short = 'u', long, default_value = "http://127.0.0.1:8642")]
    url: String,

    /// Optional API key for authentication
    #[arg(short = 'k', long)]
    key: Option<String>,

    /// Username
    #[arg(short = 'U', long, default_value_t = whoami())]
    user: String,

    /// Session ID to resume (omit to create a new session)
    #[arg(short = 's', long)]
    session: Option<String>,
}

fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "anonymous".into())
}

// ---------------------------------------------------------------------------
// Gateway API types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GatewaySession {
    id: String,
    user: String,
    created_at: String,
    last_active: String,
    message_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GatewayStatus {
    running: bool,
    listen: String,
    mode: String,
    uptime_secs: u64,
    active_sessions: usize,
    total_requests: u64,
    tailscale_ip: Option<String>,
    version: String,
}

// ---------------------------------------------------------------------------
// Gateway HTTP client
// ---------------------------------------------------------------------------

struct GatewayClient {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
}

impl GatewayClient {
    fn new(base_url: &str, api_key: Option<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
        }
    }

    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key {
            Some(key) => req.bearer_auth(key),
            None => req,
        }
    }

    async fn check_status(&self) -> Result<GatewayStatus> {
        let url = format!("{}/v1/status", self.base_url);
        let req = self.auth(self.http.get(&url).timeout(Duration::from_secs(10)));
        let resp = req.send().await.context("Failed to reach gateway")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Gateway HTTP {}: {}", status.as_u16(), body);
        }
        let gw: GatewayStatus = resp.json().await.context("Bad status JSON")?;
        Ok(gw)
    }

    async fn create_session(&self, user: &str) -> Result<GatewaySession> {
        let url = format!("{}/v1/sessions", self.base_url);
        let req = self.auth(
            self.http
                .post(&url)
                .json(&serde_json::json!({ "user": user }))
                .timeout(Duration::from_secs(15)),
        );
        let resp = req.send().await.context("Failed to create session")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Create session HTTP {}: {}", status.as_u16(), body);
        }
        // The gateway wraps the session in a {"status":"Created","data":{...}} envelope
        let json: serde_json::Value = resp.json().await.context("Bad session JSON")?;
        // Try to extract from data field first, then try top-level
        let session_val = json.get("data").unwrap_or(&json);
        let session: GatewaySession =
            serde_json::from_value(session_val.clone()).context("Cannot parse session")?;
        Ok(session)
    }

    async fn chat(&self, message: &str, session_id: &str) -> Result<(String, String)> {
        let url = format!("{}/v1/chat", self.base_url);
        let body = serde_json::json!({
            "message": message,
            "session_id": session_id,
        });
        let req = self.auth(
            self.http
                .post(&url)
                .json(&body)
                .timeout(Duration::from_secs(120)),
        );
        let resp = req.send().await.context("Failed to send chat")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Chat HTTP {}: {}", status.as_u16(), body);
        }
        let json: serde_json::Value = resp.json().await.context("Bad chat JSON")?;
        let data = json.get("data").unwrap_or(&json);
        let response_text = data
            .get("response")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let sid = data
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or(session_id)
            .to_string();
        Ok((response_text, sid))
    }
}

// ---------------------------------------------------------------------------
// Chat message model
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum ChatRole {
    User,
    Assistant,
    System,
}

#[derive(Clone)]
struct ChatMessage {
    role: ChatRole,
    content: String,
}

// ---------------------------------------------------------------------------
// Application state
// ---------------------------------------------------------------------------

enum ConnState {
    Connecting,
    Connected,
    Error(String),
}

struct App {
    messages: Vec<ChatMessage>,
    input: String,
    input_history: Vec<String>,
    history_index: Option<usize>,
    cursor_pos: usize,
    scroll_offset: u16,
    session_id: String,
    conn_state: ConnState,
    gateway_url: String,
    show_help: bool,
    waiting: bool,
    tick: usize,
    should_quit: bool,
}

impl App {
    fn new(gateway_url: &str) -> Self {
        Self {
            messages: Vec::new(),
            input: String::new(),
            input_history: Vec::new(),
            history_index: None,
            cursor_pos: 0,
            scroll_offset: 0,
            session_id: String::new(),
            conn_state: ConnState::Connecting,
            gateway_url: gateway_url.to_string(),
            show_help: false,
            waiting: false,
            tick: 0,
            should_quit: false,
        }
    }

    fn push_system(&mut self, msg: &str) {
        self.messages.push(ChatMessage {
            role: ChatRole::System,
            content: msg.to_string(),
        });
    }

    fn push_user(&mut self, msg: &str) {
        self.messages.push(ChatMessage {
            role: ChatRole::User,
            content: msg.to_string(),
        });
    }

    fn push_assistant(&mut self, msg: &str) {
        self.messages.push(ChatMessage {
            role: ChatRole::Assistant,
            content: msg.to_string(),
        });
    }

    fn submit_input(&mut self) -> Option<String> {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return None;
        }
        self.input_history.push(text.clone());
        self.history_index = None;
        self.input.clear();
        self.cursor_pos = 0;
        Some(text)
    }

    fn history_up(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        let idx = match self.history_index {
            Some(i) if i > 0 => i - 1,
            Some(i) => i,
            None => self.input_history.len() - 1,
        };
        self.history_index = Some(idx);
        self.input = self.input_history[idx].clone();
        self.cursor_pos = self.input.len();
    }

    fn history_down(&mut self) {
        match self.history_index {
            Some(i) => {
                if i + 1 < self.input_history.len() {
                    let idx = i + 1;
                    self.history_index = Some(idx);
                    self.input = self.input_history[idx].clone();
                    self.cursor_pos = self.input.len();
                } else {
                    self.history_index = None;
                    self.input.clear();
                    self.cursor_pos = 0;
                }
            }
            None => {}
        }
    }

    fn scroll_up(&mut self, amount: u16) {
        self.scroll_offset = self.scroll_offset.saturating_add(amount);
    }

    fn scroll_down(&mut self, amount: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
    }

    fn status_text(&self) -> String {
        let conn = match &self.conn_state {
            ConnState::Connecting => "CONNECTING...".to_string(),
            ConnState::Connected => "CONNECTED".to_string(),
            ConnState::Error(e) => format!("ERROR: {}", e),
        };
        let thinking = if self.waiting {
            let dots = ".".repeat((self.tick / 3) % 4);
            format!("  Thinking{:<3}", dots)
        } else {
            String::new()
        };
        let sid = if self.session_id.is_empty() {
            "no session".to_string()
        } else {
            self.session_id.clone()
        };
        format!(
            " {} | {} | {} {}",
            conn, sid, self.gateway_url, thinking
        )
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),    // chat history
            Constraint::Length(1), // status bar
            Constraint::Length(3), // input
        ])
        .split(f.area());

    draw_chat(f, app, chunks[0]);
    draw_status_bar(f, app, chunks[1]);
    draw_input(f, app, chunks[2]);

    if app.show_help {
        draw_help_overlay(f, f.area());
    }
}

fn draw_chat(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();

    for msg in &app.messages {
        let (prefix, style) = match msg.role {
            ChatRole::User => (
                "You: ",
                Style::default().fg(Color::Cyan),
            ),
            ChatRole::Assistant => (
                "AI:  ",
                Style::default().fg(Color::Green),
            ),
            ChatRole::System => (
                " *** ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::ITALIC),
            ),
        };

        // Split content into display lines
        for (i, text_line) in msg.content.lines().enumerate() {
            let p = if i == 0 { prefix } else { "     " };
            lines.push(Line::from(vec![
                Span::styled(p, style.add_modifier(Modifier::BOLD)),
                Span::styled(text_line.to_string(), style),
            ]));
        }
        // Blank line between messages
        lines.push(Line::from(""));
    }

    // Auto-scroll: if offset is 0 (bottom), clamp so newest messages are visible
    let visible_height = area.height.saturating_sub(2) as usize; // borders
    let total = lines.len();
    let scroll = if app.scroll_offset == 0 {
        total.saturating_sub(visible_height) as u16
    } else {
        let max_scroll = total.saturating_sub(visible_height) as u16;
        max_scroll.saturating_sub(app.scroll_offset)
    };

    let chat_block = Block::default()
        .borders(Borders::ALL)
        .title(" bfcode Chat ");

    let paragraph = Paragraph::new(Text::from(lines))
        .block(chat_block)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    f.render_widget(paragraph, area);
}

fn draw_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let status = app.status_text();
    let style = match &app.conn_state {
        ConnState::Connected => Style::default().bg(Color::DarkGray).fg(Color::White),
        ConnState::Connecting => Style::default().bg(Color::Blue).fg(Color::White),
        ConnState::Error(_) => Style::default().bg(Color::Red).fg(Color::White),
    };
    let bar = Paragraph::new(Line::from(status)).style(style);
    f.render_widget(bar, area);
}

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Message (Enter=send, Ctrl+Enter=newline, Ctrl+/=help) ");

    let display_text = if app.input.is_empty() && !app.waiting {
        "Type a message...".to_string()
    } else {
        app.input.clone()
    };

    let style = if app.input.is_empty() && !app.waiting {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White)
    };

    let paragraph = Paragraph::new(Text::from(display_text))
        .block(block)
        .style(style)
        .wrap(Wrap { trim: false });

    f.render_widget(paragraph, area);

    // Place cursor
    if !app.waiting {
        let cx = area.x + 1 + app.cursor_pos as u16;
        let cy = area.y + 1;
        f.set_cursor_position((cx.min(area.x + area.width - 2), cy));
    }
}

fn draw_help_overlay(f: &mut Frame, area: Rect) {
    let help_text = vec![
        Line::from(Span::styled(
            " Keyboard Shortcuts ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("  Enter          Send message"),
        Line::from("  Ctrl+Enter     Insert newline"),
        Line::from("  Up / Down      Input history"),
        Line::from("  PageUp/Down    Scroll chat"),
        Line::from("  Ctrl+C / Esc   Quit"),
        Line::from("  Ctrl+/         Toggle this help"),
        Line::from(""),
        Line::from(Span::styled(
            " Press any key to close ",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let width = 44u16;
    let height = help_text.len() as u16 + 2;
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let popup_area = Rect::new(x, y, width.min(area.width), height.min(area.height));

    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Black).fg(Color::White))
        .title(" Help ");

    let paragraph = Paragraph::new(Text::from(help_text)).block(block);
    // Clear background
    f.render_widget(ratatui::widgets::Clear, popup_area);
    f.render_widget(paragraph, popup_area);
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let gw = GatewayClient::new(&args.url, args.key.clone());
    let mut app = App::new(&args.url);

    // --- Set up terminal ---
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Ensure cleanup on exit
    let result = run_app(&mut terminal, &mut app, &gw, &args).await;

    // --- Restore terminal ---
    terminal::disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    gw: &GatewayClient,
    args: &Args,
) -> Result<()> {
    // --- Initial connection ---
    app.push_system(&format!("Connecting to {}...", args.url));
    terminal.draw(|f| draw(f, app))?;

    // Check gateway status
    match gw.check_status().await {
        Ok(status) => {
            app.conn_state = ConnState::Connected;
            app.push_system(&format!(
                "Connected to gateway v{} (mode: {}, uptime: {}s, sessions: {})",
                status.version, status.mode, status.uptime_secs, status.active_sessions
            ));
        }
        Err(e) => {
            app.conn_state = ConnState::Error(format!("{:#}", e));
            app.push_system(&format!("Connection failed: {:#}", e));
            app.push_system("You can still try sending messages. They will attempt to connect.");
        }
    }

    // Create or resume session
    match &args.session {
        Some(sid) => {
            app.session_id = sid.clone();
            app.push_system(&format!("Resuming session: {}", sid));
        }
        None => match gw.create_session(&args.user).await {
            Ok(session) => {
                app.session_id = session.id.clone();
                app.conn_state = ConnState::Connected;
                app.push_system(&format!(
                    "Session created: {} (user: {})",
                    session.id, session.user
                ));
            }
            Err(e) => {
                app.push_system(&format!("Could not create session: {:#}", e));
                app.push_system("Will auto-create session on first message.");
            }
        },
    }

    terminal.draw(|f| draw(f, app))?;

    // --- Event loop ---
    loop {
        // Poll for events with timeout so we can animate
        if poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) => {
                    if app.show_help {
                        // Any key closes help
                        app.show_help = false;
                    } else {
                        match handle_key(app, key) {
                            KeyAction::Quit => break,
                            KeyAction::Send(msg) => {
                                app.push_user(&msg);
                                app.scroll_offset = 0;
                                app.waiting = true;
                                // Draw immediately to show user message
                                terminal.draw(|f| draw(f, app))?;

                                // Send to gateway
                                match gw.chat(&msg, &app.session_id).await {
                                    Ok((response, sid)) => {
                                        if !sid.is_empty() {
                                            app.session_id = sid;
                                        }
                                        app.conn_state = ConnState::Connected;
                                        app.push_assistant(&response);
                                    }
                                    Err(e) => {
                                        let err_msg = format!("Error: {:#}", e);
                                        app.conn_state =
                                            ConnState::Error(format!("{:#}", e));
                                        app.push_system(&err_msg);
                                    }
                                }
                                app.waiting = false;
                                app.scroll_offset = 0;
                            }
                            KeyAction::None => {}
                        }
                    }
                }
                Event::Resize(_, _) => {
                    // Terminal will redraw
                }
                _ => {}
            }
        }

        // Tick for animation
        app.tick = app.tick.wrapping_add(1);
        terminal.draw(|f| draw(f, app))?;

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

enum KeyAction {
    None,
    Quit,
    Send(String),
}

fn handle_key(app: &mut App, key: KeyEvent) -> KeyAction {
    match (key.modifiers, key.code) {
        // Quit
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => return KeyAction::Quit,
        (_, KeyCode::Esc) => return KeyAction::Quit,

        // Help toggle
        (KeyModifiers::CONTROL, KeyCode::Char('/')) => {
            app.show_help = !app.show_help;
        }

        // Ctrl+Enter -> newline in input
        (KeyModifiers::CONTROL, KeyCode::Enter) => {
            app.input.insert(app.cursor_pos, '\n');
            app.cursor_pos += 1;
        }

        // Enter -> send
        (_, KeyCode::Enter) if !app.waiting => {
            if let Some(msg) = app.submit_input() {
                return KeyAction::Send(msg);
            }
        }

        // Input history
        (_, KeyCode::Up) => {
            app.history_up();
        }
        (_, KeyCode::Down) => {
            app.history_down();
        }

        // Scroll chat
        (_, KeyCode::PageUp) => {
            app.scroll_up(5);
        }
        (_, KeyCode::PageDown) => {
            app.scroll_down(5);
        }

        // Backspace
        (_, KeyCode::Backspace) if app.cursor_pos > 0 => {
            app.cursor_pos -= 1;
            app.input.remove(app.cursor_pos);
        }

        // Delete
        (_, KeyCode::Delete) if app.cursor_pos < app.input.len() => {
            app.input.remove(app.cursor_pos);
        }

        // Left / Right
        (_, KeyCode::Left) if app.cursor_pos > 0 => {
            app.cursor_pos -= 1;
        }
        (_, KeyCode::Right) if app.cursor_pos < app.input.len() => {
            app.cursor_pos += 1;
        }

        // Home / End
        (_, KeyCode::Home) => {
            app.cursor_pos = 0;
        }
        (_, KeyCode::End) => {
            app.cursor_pos = app.input.len();
        }

        // Character input
        (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(c)) if !app.waiting => {
            app.input.insert(app.cursor_pos, c);
            app.cursor_pos += 1;
        }

        _ => {}
    }

    KeyAction::None
}
