//! Simple REPL client for connecting to a bfcode gateway server.
//!
//! Usage:
//!   bfcode_client --url http://127.0.0.1:8642
//!   bfcode_client -u http://myserver:9000 -k myapikey

use anyhow::{Context, Result, bail};
use clap::Parser;
use colored::Colorize;
use std::io::{self, Write};
use std::time::Duration;

// ---------------------------------------------------------------------------
// CLI arguments
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "bfcode-client", about = "REPL client for bfcode gateway")]
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

    async fn check_status(&self) -> Result<serde_json::Value> {
        let url = format!("{}/v1/status", self.base_url);
        let req = self.auth(self.http.get(&url).timeout(Duration::from_secs(10)));
        let resp = req.send().await.context("Failed to reach gateway")?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Gateway HTTP {}", body);
        }
        Ok(resp.json().await.context("Bad status JSON")?)
    }

    async fn create_session(&self, user: &str) -> Result<String> {
        let url = format!("{}/v1/sessions", self.base_url);
        let req = self.auth(
            self.http
                .post(&url)
                .json(&serde_json::json!({ "user": user }))
                .timeout(Duration::from_secs(15)),
        );
        let resp = req.send().await.context("Failed to create session")?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Create session failed: {}", body);
        }
        let json: serde_json::Value = resp.json().await?;
        let sid = json
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        Ok(sid)
    }

    async fn chat(&self, message: &str, session_id: &str) -> Result<ChatResult> {
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
        let resp = req.send().await.context("Failed to send message")?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Chat error: {}", body);
        }
        let json: serde_json::Value = resp.json().await?;
        let response = json
            .get("response")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        Ok(ChatResult {
            response,
            prompt_tokens: json.get("prompt_tokens").and_then(|v| v.as_u64()),
            completion_tokens: json.get("completion_tokens").and_then(|v| v.as_u64()),
            total_tokens: json.get("total_tokens").and_then(|v| v.as_u64()),
            session_tokens: json.get("session_tokens").and_then(|v| v.as_u64()),
            cost: json.get("cost").and_then(|v| v.as_f64()),
            model: json.get("model").and_then(|v| v.as_str()).map(String::from),
        })
    }
}

// ---------------------------------------------------------------------------
// Chat result
// ---------------------------------------------------------------------------

struct ChatResult {
    response: String,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
    session_tokens: Option<u64>,
    cost: Option<f64>,
    model: Option<String>,
}

// ---------------------------------------------------------------------------
// Spinner
// ---------------------------------------------------------------------------

/// Run a future while displaying a spinner animation on the current line.
async fn run_with_spinner<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

    tokio::pin!(fut);

    let mut frame_idx = 0usize;
    loop {
        tokio::select! {
            result = &mut fut => {
                // Clear spinner line
                print!("\r{}\r", " ".repeat(30));
                io::stdout().flush().ok();
                return result;
            }
            _ = tokio::time::sleep(Duration::from_millis(80)) => {
                let spinner = FRAMES[frame_idx % FRAMES.len()];
                print!("\r{} {}", spinner.cyan().to_string(), "Thinking...".dimmed());
                io::stdout().flush().ok();
                frame_idx += 1;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let gw = GatewayClient::new(&args.url, args.key.clone());

    println!(
        "{} connecting to {}",
        "bfcode-client".cyan().bold(),
        args.url.green()
    );

    // Check gateway status
    match gw.check_status().await {
        Ok(status) => {
            let version = status.get("version").and_then(|v| v.as_str()).unwrap_or("?");
            let mode = status.get("mode").and_then(|v| v.as_str()).unwrap_or("?");
            let uptime = status.get("uptime_secs").and_then(|v| v.as_u64()).unwrap_or(0);
            println!(
                "{} gateway v{} (mode: {}, uptime: {}s)",
                "bfcode-client".cyan().bold(),
                version.yellow(),
                mode,
                uptime
            );
        }
        Err(e) => {
            eprintln!(
                "{} connection failed: {}",
                "bfcode-client".cyan().bold(),
                format!("{:#}", e).red()
            );
            eprintln!(
                "{} messages will attempt to connect on send",
                "bfcode-client".cyan().bold()
            );
        }
    }

    // Create or resume session
    let mut session_id = match &args.session {
        Some(sid) => {
            println!(
                "{} resuming session: {}",
                "bfcode-client".cyan().bold(),
                sid.yellow()
            );
            sid.clone()
        }
        None => match gw.create_session(&args.user).await {
            Ok(sid) => {
                println!(
                    "{} session: {} (user: {})",
                    "bfcode-client".cyan().bold(),
                    sid.yellow(),
                    args.user
                );
                sid
            }
            Err(e) => {
                eprintln!(
                    "{} could not create session: {}",
                    "bfcode-client".cyan().bold(),
                    format!("{:#}", e).red()
                );
                String::new()
            }
        },
    };

    println!();
    println!(
        "Type a message and press Enter. Use {} to quit. {} for multiline (end with empty line).",
        "Ctrl+C".yellow(),
        "\\\\".yellow()
    );
    println!();

    let stdin = io::stdin();
    loop {
        print!("{} ", ">".cyan().bold());
        io::stdout().flush()?;

        let mut input = String::new();
        if stdin.read_line(&mut input)? == 0 {
            println!("\nGoodbye!");
            break;
        }

        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        // Multiline mode: if line ends with \, keep reading until empty line
        let message = if input.ends_with('\\') {
            let mut lines = vec![input.trim_end_matches('\\').to_string()];
            loop {
                print!("{} ", "..".dimmed());
                io::stdout().flush()?;
                let mut line = String::new();
                if stdin.read_line(&mut line)? == 0 {
                    break;
                }
                let line = line.trim_end_matches('\n').trim_end_matches('\r');
                if line.is_empty() {
                    break;
                }
                lines.push(line.to_string());
            }
            lines.join("\n")
        } else {
            input.to_string()
        };

        if message.is_empty() {
            continue;
        }

        // Slash commands
        if message == "/quit" || message == "/exit" {
            println!("Goodbye!");
            break;
        }
        if message == "/help" {
            println!();
            println!("  {}        Send message (single line)", "Enter".yellow());
            println!(
                "  {}    Multiline: end line with \\, then empty line to send",
                "\\\\".yellow()
            );
            println!("  {}   Quit", "/quit".yellow());
            println!("  {}   Show this help", "/help".yellow());
            println!("  {} Show session info", "/status".yellow());
            println!();
            continue;
        }
        if message == "/status" {
            match gw.check_status().await {
                Ok(status) => println!("{}", serde_json::to_string_pretty(&status)?),
                Err(e) => eprintln!("{}", format!("Error: {:#}", e).red()),
            }
            continue;
        }

        // Send message with spinner animation
        let chat_fut = gw.chat(&message, &session_id);
        let result = run_with_spinner(chat_fut).await;

        match result {
            Ok(chat) => {
                println!("{} {}", "AI:".green().bold(), chat.response);
                // Show token usage and cost
                if let Some(total) = chat.total_tokens {
                    let model_name = chat.model.as_deref().unwrap_or("unknown");
                    let prompt = chat.prompt_tokens.unwrap_or(0);
                    let completion = chat.completion_tokens.unwrap_or(0);
                    let cost = chat.cost.unwrap_or(0.0);
                    let session_info = chat
                        .session_tokens
                        .map(|st| format!(" | session: {} tokens", st))
                        .unwrap_or_default();
                    eprintln!(
                        "  {} {} tokens ({}in/{}out) ${:.4} [{}]{}",
                        "~".dimmed(),
                        total.to_string().dimmed(),
                        prompt.to_string().dimmed(),
                        completion.to_string().dimmed(),
                        cost,
                        model_name.dimmed(),
                        session_info.dimmed(),
                    );
                }
            }
            Err(e) => {
                eprintln!("{} {}", "Error:".red().bold(), format!("{:#}", e).red());
                // Try to create session if we don't have one
                if session_id.is_empty() {
                    if let Ok(sid) = gw.create_session(&args.user).await {
                        session_id = sid;
                    }
                }
            }
        }
        println!();
    }

    Ok(())
}
