mod api;
mod persistence;
mod tools;
mod types;

use colored::Colorize;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use types::{GlobalConfig, Message, ProjectSession};

const MAX_TOOL_ROUNDS: usize = 25;

// Spinner frames (braille pattern)
const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GROK_API_KEY").expect("GROK_API_KEY environment variable not set");

    // Load global config
    let mut config = persistence::load_config();

    // Load project session
    let mut session = persistence::load_session();

    // Load project instructions (AGENTS.md, CLAUDE.md, BFCODE.md, etc.)
    let instructions = persistence::load_instructions();

    // Load existing plans as context
    let plans_context = persistence::load_plans_context();

    // Build full system prompt = base + instructions + plans
    let mut full_system_prompt = config.system_prompt.clone();
    if let Some(ref instr) = instructions {
        full_system_prompt.push_str(instr);
    }
    if let Some(ref plans) = plans_context {
        full_system_prompt.push_str(plans);
    }

    // Ensure system prompt is first message
    if session.conversation.is_empty() || session.conversation[0].role != "system" {
        session
            .conversation
            .insert(0, Message::system(&full_system_prompt));
    }

    let client = api::GrokClient::new(api_key);
    let tool_defs = tools::get_tool_definitions();
    let permissions = tools::Permissions::new();

    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".into());

    // Welcome banner
    println!("{}", "bfcode v0.5.0".green().bold());
    println!("Project:  {}", cwd.dimmed());
    println!("Model:    {}", config.model.cyan());
    println!(
        "Session:  {} ({})",
        session.id.cyan(),
        session.title.dimmed()
    );
    if let Some(ref instr) = instructions {
        // Extract filename from first line
        let file_hint = instr
            .lines()
            .find(|l| l.contains("from "))
            .unwrap_or("project instructions");
        println!("Loaded:   {}", file_hint.dimmed());
    }
    println!();
    println!("Type {} for commands", "/help".yellow());
    println!();

    let stdin = std::io::stdin();
    loop {
        print!("{} ", ">".cyan().bold());
        std::io::stdout().flush()?;

        let mut input = String::new();
        if stdin.read_line(&mut input)? == 0 {
            println!("\nGoodbye!");
            break;
        }

        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        // Handle slash commands
        if input.starts_with('/') {
            let handled = handle_command(input, &mut session, &mut config, &full_system_prompt)?;
            if handled == CommandResult::Quit {
                break;
            }
            continue;
        }

        // Auto-set session title from first user message
        if session.title == "New session" {
            let title: String = input.chars().take(50).collect();
            session.title = title;
        }
        session.updated_at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

        session.conversation.push(Message::user(input));

        // Agent loop
        let mut error_occurred = false;
        for _round in 0..MAX_TOOL_ROUNDS {
            // Start spinner
            let spinning = Arc::new(AtomicBool::new(true));
            let spinner_handle = start_spinner(spinning.clone());

            let response = client
                .chat(
                    &session.conversation,
                    &tool_defs,
                    &config.model,
                    config.temperature,
                )
                .await;

            // Stop spinner
            spinning.store(false, Ordering::Relaxed);
            let _ = spinner_handle.await;
            eprint!("\r\x1b[K"); // Clear spinner line

            let response = match response {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("{} {e}", "Error:".red().bold());
                    error_occurred = true;
                    break;
                }
            };

            // Track tokens
            if let Some(usage) = &response.usage {
                session.total_tokens += usage.total_tokens;
            }

            if response.choices.is_empty() {
                eprintln!("{} Empty response from API", "Error:".red().bold());
                error_occurred = true;
                break;
            }

            let assistant_msg = &response.choices[0].message;

            // Handle tool calls
            if let Some(tool_calls) = &assistant_msg.tool_calls {
                session
                    .conversation
                    .push(Message::assistant_tool_calls(tool_calls.clone()));

                for tc in tool_calls {
                    tools::print_tool_call(&tc.function.name, &tc.function.arguments);
                    let result = tools::execute_tool(
                        &tc.function.name,
                        &tc.function.arguments,
                        &permissions,
                    )
                    .await;
                    tools::print_tool_result(&result);
                    session
                        .conversation
                        .push(Message::tool_result(&tc.id, &result));
                }
                continue;
            }

            // Text response
            if let Some(content) = &assistant_msg.content {
                session.conversation.push(Message::assistant_text(content));
                println!("\n{}\n", content);
            }

            // Show token usage
            if let Some(usage) = &response.usage {
                eprintln!(
                    "  {} tokens: {} this call | {} session total",
                    "~".dimmed(),
                    format_tokens(usage.total_tokens).dimmed(),
                    format_tokens(session.total_tokens).dimmed(),
                );
            }

            break;
        }

        // Remove last user message on error
        if error_occurred && session.conversation.last().map(|m| m.role.as_str()) == Some("user") {
            session.conversation.pop();
        }

        persistence::save_session(&session)?;
    }

    Ok(())
}

// --- Slash command handling ---

#[derive(PartialEq)]
enum CommandResult {
    Continue,
    Quit,
}

fn handle_command(
    input: &str,
    session: &mut ProjectSession,
    config: &mut GlobalConfig,
    full_system_prompt: &str,
) -> Result<CommandResult, Box<dyn std::error::Error>> {
    let parts: Vec<&str> = input.splitn(2, ' ').collect();
    let cmd = parts[0];
    let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");

    match cmd {
        "/quit" | "/exit" | "/q" => {
            println!("Goodbye!");
            return Ok(CommandResult::Quit);
        }
        "/help" | "/h" => {
            println!("{}", "Commands:".yellow().bold());
            println!("  {}         - show this help", "/help".yellow());
            println!("  {}        - clear current session", "/clear".yellow());
            println!("  {}      - compact conversation", "/compact".yellow());
            println!("  {}          - create a new session", "/new".yellow());
            println!("  {}     - list all sessions", "/sessions".yellow());
            println!("  {}   - switch to session by ID", "/resume <id>".yellow());
            println!("  {}  - change model", "/model <name>".yellow());
            println!("  {} - save a plan as .md file", "/plan <name>".yellow());
            println!("  {}        - list saved plans", "/plans".yellow());
            println!("  {}         - exit", "/quit".yellow());
        }
        "/clear" => {
            persistence::clear_session(session);
            session
                .conversation
                .insert(0, Message::system(full_system_prompt));
            persistence::save_session(session)?;
            println!("{}", "Session cleared.".yellow());
        }
        "/compact" => {
            let before = session.conversation.len();
            compact_conversation(session, full_system_prompt);
            persistence::save_session(session)?;
            println!(
                "{}",
                format!(
                    "Compacted: {before} -> {} messages",
                    session.conversation.len()
                )
                .yellow()
            );
        }
        "/new" => {
            *session = persistence::new_session();
            session
                .conversation
                .insert(0, Message::system(full_system_prompt));
            persistence::save_session(session)?;
            println!("{}", format!("New session: {}", session.id).green());
        }
        "/sessions" => {
            let sessions = persistence::list_sessions();
            if sessions.is_empty() {
                println!("{}", "No sessions found.".dimmed());
            } else {
                println!("{}", "Sessions:".yellow().bold());
                for (id, title, updated, msgs) in &sessions {
                    let marker = if *id == session.id { " *" } else { "  " };
                    println!(
                        "{} {} {} ({} msgs, {})",
                        marker.green(),
                        id.cyan(),
                        title,
                        msgs,
                        updated.dimmed()
                    );
                }
            }
        }
        "/resume" => {
            if arg.is_empty() {
                println!("{}", "Usage: /resume <session-id>".yellow());
            } else {
                match persistence::switch_session(arg) {
                    Some(loaded) => {
                        *session = loaded;
                        println!(
                            "{}",
                            format!("Resumed session: {} ({})", session.id, session.title).green()
                        );
                    }
                    None => {
                        println!("{}", format!("Session '{arg}' not found.").red());
                    }
                }
            }
        }
        "/model" => {
            if arg.is_empty() {
                println!("Current model: {}", config.model.cyan());
            } else {
                config.model = arg.to_string();
                let _ = persistence::save_config(config);
                println!("{}", format!("Model set to: {}", arg).green());
            }
        }
        "/plan" => {
            if arg.is_empty() {
                println!("{}", "Usage: /plan <name>".yellow());
                println!(
                    "{}",
                    "Then type your plan content. End with an empty line.".dimmed()
                );
            } else {
                println!("{}", "Enter plan content (empty line to finish):".yellow());
                let mut content = String::new();
                let stdin = std::io::stdin();
                loop {
                    let mut line = String::new();
                    if stdin.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    if line.trim().is_empty() {
                        break;
                    }
                    content.push_str(&line);
                }
                if content.trim().is_empty() {
                    println!("{}", "Plan content is empty, not saved.".red());
                } else {
                    match persistence::save_plan(arg, &content) {
                        Ok(path) => {
                            println!("{}", format!("Plan saved: {}", path.display()).green());
                        }
                        Err(e) => {
                            println!("{}", format!("Failed to save plan: {e}").red());
                        }
                    }
                }
            }
        }
        "/plans" => {
            let plans = persistence::list_plans();
            if plans.is_empty() {
                println!("{}", "No plans found.".dimmed());
            } else {
                println!("{}", "Plans:".yellow().bold());
                for (name, path) in &plans {
                    println!("  {} {}", name.cyan(), path.dimmed());
                }
            }
        }
        _ => {
            println!("{}", format!("Unknown command: {cmd}. Type /help").red());
        }
    }

    Ok(CommandResult::Continue)
}

// --- Spinner ---

fn start_spinner(running: Arc<AtomicBool>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let start = std::time::Instant::now();
        let mut i = 0;
        while running.load(Ordering::Relaxed) {
            let elapsed = start.elapsed().as_secs();
            let frame = SPINNER[i % SPINNER.len()];
            eprint!("\r  {} thinking... {}s", frame.cyan(), elapsed);
            let _ = std::io::stderr().flush();
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            i += 1;
        }
    })
}

// --- Helpers ---

fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        format!("{tokens}")
    }
}

fn compact_conversation(session: &mut ProjectSession, full_system_prompt: &str) {
    let messages = &session.conversation;
    if messages.len() <= 10 {
        return;
    }

    let mut compacted = Vec::new();

    compacted.push(Message::system(full_system_prompt));

    let total = messages.len();
    compacted.push(Message::system(&format!(
        "[Previous conversation compacted. {total} messages summarized. Continue assisting the user.]"
    )));

    let keep_from = messages.len().saturating_sub(8);
    for msg in &messages[keep_from..] {
        if msg.role != "system" {
            compacted.push(msg.clone());
        }
    }

    session.conversation = compacted;
}
