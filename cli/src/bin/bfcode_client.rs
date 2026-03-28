//! WebSocket REPL client for connecting to a bfcode gateway server.
//!
//! Usage:
//!   bfcode-cli --url ws://127.0.0.1:8642/v1/ws
//!   bfcode-cli -u ws://myserver:9000/v1/ws -k myapikey

use anyhow::{Context, Result, bail};
use clap::Parser;
use colored::Colorize;
use futures_util::{SinkExt, StreamExt};
use std::io::{self, Write};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

// ---------------------------------------------------------------------------
// CLI arguments
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "bfcode-cli", about = "WebSocket REPL client for bfcode gateway")]
struct Args {
    /// Gateway URL (HTTP base or WebSocket endpoint)
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

/// Convert an HTTP base URL to a WebSocket URL for /v1/ws
fn to_ws_url(base: &str) -> String {
    let base = base.trim_end_matches('/');
    // If already a ws:// or wss:// URL, use as-is
    if base.starts_with("ws://") || base.starts_with("wss://") {
        if base.ends_with("/v1/ws") {
            return base.to_string();
        }
        return format!("{base}/v1/ws");
    }
    // Convert http(s) to ws(s)
    let ws_base = if base.starts_with("https://") {
        base.replacen("https://", "wss://", 1)
    } else if base.starts_with("http://") {
        base.replacen("http://", "ws://", 1)
    } else {
        format!("ws://{base}")
    };
    if ws_base.ends_with("/v1/ws") {
        ws_base
    } else {
        format!("{ws_base}/v1/ws")
    }
}

/// Convert an HTTP base URL to the status endpoint
fn to_status_url(base: &str) -> String {
    let base = base.trim_end_matches('/');
    // Strip /v1/ws if present for HTTP status check
    let http_base = if base.starts_with("ws://") {
        base.replacen("ws://", "http://", 1)
    } else if base.starts_with("wss://") {
        base.replacen("wss://", "https://", 1)
    } else {
        base.to_string()
    };
    let http_base = http_base.trim_end_matches("/v1/ws");
    format!("{http_base}/v1/status")
}

// ---------------------------------------------------------------------------
// Spinner
// ---------------------------------------------------------------------------

async fn run_with_spinner<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

    tokio::pin!(fut);

    let start = std::time::Instant::now();
    let mut frame_idx = 0usize;
    loop {
        tokio::select! {
            result = &mut fut => {
                let elapsed = start.elapsed();
                print!("\r{}\r", " ".repeat(50));
                io::stdout().flush().ok();
                if elapsed.as_secs() >= 1 {
                    eprintln!("  {} {:.1}s", "~".dimmed(), elapsed.as_secs_f64());
                }
                return result;
            }
            _ = tokio::time::sleep(Duration::from_millis(80)) => {
                let elapsed = start.elapsed().as_secs();
                let spinner = FRAMES[frame_idx % FRAMES.len()];
                let msg = if elapsed < 3 {
                    "Thinking...".to_string()
                } else if elapsed < 10 {
                    format!("Thinking... {}s", elapsed)
                } else if elapsed < 30 {
                    format!("Still working... {}s", elapsed)
                } else {
                    format!("Processing ({}s)...", elapsed)
                };
                print!("\r{} {}", spinner.cyan().to_string(), msg.dimmed());
                io::stdout().flush().ok();
                frame_idx += 1;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// WebSocket client
// ---------------------------------------------------------------------------

type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    WsMessage,
>;
type WsStream = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

async fn connect_ws(ws_url: &str, api_key: Option<&str>) -> Result<(WsSink, WsStream)> {
    let mut request = ws_url
        .into_client_request()
        .context("Invalid WebSocket URL")?;

    if let Some(key) = api_key {
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {key}")
                .parse()
                .context("Invalid API key header")?,
        );
    }

    let (ws_stream, _response) = tokio_tungstenite::connect_async(request)
        .await
        .context("Failed to connect WebSocket")?;

    Ok(ws_stream.split())
}

/// Send a JSON message over WebSocket and wait for the response.
/// Skips intermediate status messages like "thinking".
async fn ws_send_recv(
    sink: &mut WsSink,
    stream: &mut WsStream,
    payload: serde_json::Value,
) -> Result<serde_json::Value> {
    let text = payload.to_string();
    sink.send(WsMessage::Text(text.into()))
        .await
        .context("Failed to send WebSocket message")?;

    // Wait for response with timeout, skipping "thinking" acks
    let resp = tokio::time::timeout(Duration::from_secs(120), async {
        while let Some(msg) = stream.next().await {
            match msg {
                Ok(WsMessage::Text(text)) => {
                    let json: serde_json::Value =
                        serde_json::from_str(&text).context("Invalid JSON from server")?;
                    // Skip intermediate status messages (thinking, etc.)
                    let msg_type = json.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    if msg_type == "thinking" {
                        continue;
                    }
                    return Ok(json);
                }
                Ok(WsMessage::Ping(_)) => continue,
                Ok(WsMessage::Close(_)) => bail!("Server closed connection"),
                Err(e) => bail!("WebSocket error: {e}"),
                _ => continue,
            }
        }
        bail!("WebSocket stream ended unexpectedly")
    })
    .await
    .context("Response timed out after 120 seconds")??;

    Ok(resp)
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let ws_url = to_ws_url(&args.url);

    println!(
        "{} connecting via WebSocket to {}",
        "bfcode-cli".cyan().bold(),
        ws_url.green()
    );

    // Optional: check HTTP status first
    {
        let status_url = to_status_url(&args.url);
        let http = reqwest::Client::new();
        let mut req = http.get(&status_url).timeout(Duration::from_secs(5));
        if let Some(ref key) = args.key {
            req = req.bearer_auth(key);
        }
        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(status) = resp.json::<serde_json::Value>().await {
                    let version = status.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                    let mode = status.get("mode").and_then(|v| v.as_str()).unwrap_or("?");
                    let uptime = status.get("uptime_secs").and_then(|v| v.as_u64()).unwrap_or(0);
                    println!(
                        "{} gateway v{} (mode: {}, uptime: {}s)",
                        "bfcode-cli".cyan().bold(),
                        version.yellow(),
                        mode,
                        uptime
                    );
                }
            }
            _ => {
                eprintln!(
                    "{} could not reach gateway status endpoint",
                    "bfcode-cli".cyan().bold(),
                );
            }
        }
    }

    // Connect WebSocket
    let (mut sink, mut stream) = connect_ws(&ws_url, args.key.as_deref()).await?;

    println!(
        "{} WebSocket connected",
        "bfcode-cli".cyan().bold(),
    );

    // Create session via WebSocket or use provided session ID
    let mut session_id = match &args.session {
        Some(sid) => {
            println!(
                "{} resuming session: {}",
                "bfcode-cli".cyan().bold(),
                sid.yellow()
            );
            sid.clone()
        }
        None => {
            // We'll let the server auto-create a session on first chat message
            String::new()
        }
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
            println!("  {} Show gateway status", "/status".yellow());
            println!("  {} Send ping to server", "/ping".yellow());
            println!();
            continue;
        }
        if message == "/status" {
            let payload = serde_json::json!({"type": "status"});
            match ws_send_recv(&mut sink, &mut stream, payload).await {
                Ok(resp) => println!("{}", serde_json::to_string_pretty(&resp)?),
                Err(e) => eprintln!("{}", format!("Error: {:#}", e).red()),
            }
            continue;
        }
        if message == "/ping" {
            let payload = serde_json::json!({"type": "ping"});
            match ws_send_recv(&mut sink, &mut stream, payload).await {
                Ok(resp) => {
                    let t = resp.get("type").and_then(|v| v.as_str()).unwrap_or("?");
                    println!("{}", format!("Server: {t}").green());
                }
                Err(e) => eprintln!("{}", format!("Error: {:#}", e).red()),
            }
            continue;
        }

        // Build chat request
        let mut payload = serde_json::json!({
            "type": "chat",
            "message": message,
        });
        if !session_id.is_empty() {
            payload["session_id"] = serde_json::json!(session_id);
        }

        // Send with spinner
        let chat_fut = ws_send_recv(&mut sink, &mut stream, payload);
        let result = run_with_spinner(chat_fut).await;

        match result {
            Ok(resp) => {
                let resp_type = resp.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if resp_type == "error" {
                    let err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("Unknown error");
                    eprintln!("{} {}", "Error:".red().bold(), err.red());
                } else {
                    let response = resp.get("response").and_then(|v| v.as_str()).unwrap_or("");
                    println!("{} {}", "AI:".green().bold(), response);

                    // Update session_id from response
                    if let Some(sid) = resp.get("session_id").and_then(|v| v.as_str()) {
                        if session_id.is_empty() {
                            println!(
                                "  {} session: {}",
                                "~".dimmed(),
                                sid.yellow()
                            );
                            session_id = sid.to_string();
                        }
                    }

                    // Show token usage and cost
                    if let Some(total) = resp.get("total_tokens").and_then(|v| v.as_u64()) {
                        let model_name = resp.get("model").and_then(|v| v.as_str()).unwrap_or("unknown");
                        let prompt = resp.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                        let completion = resp.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                        let cost = resp.get("cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let session_info = resp
                            .get("session_tokens")
                            .and_then(|v| v.as_u64())
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
            }
            Err(e) => {
                eprintln!("{} {}", "Error:".red().bold(), format!("{:#}", e).red());
                // Try to reconnect
                eprintln!(
                    "{} attempting to reconnect...",
                    "bfcode-cli".cyan().bold()
                );
                match connect_ws(&ws_url, args.key.as_deref()).await {
                    Ok((new_sink, new_stream)) => {
                        sink = new_sink;
                        stream = new_stream;
                        eprintln!(
                            "{} reconnected",
                            "bfcode-cli".cyan().bold()
                        );
                    }
                    Err(e) => {
                        eprintln!(
                            "{} reconnect failed: {}",
                            "bfcode-cli".cyan().bold(),
                            format!("{:#}", e).red()
                        );
                    }
                }
            }
        }
        println!();
    }

    // Close WebSocket gracefully
    let _ = sink.send(WsMessage::Close(None)).await;

    Ok(())
}
