mod api;
mod context;
mod persistence;
mod tools;
mod types;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use types::{GlobalConfig, Message, ProjectSession};

const MAX_TOOL_ROUNDS: usize = 25;

// Spinner frames (braille pattern)
const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// bfcode — back to the future code, an AI coding assistant
#[derive(Parser)]
#[command(name = "bfcode", version = "0.5.0", about = "AI coding assistant powered by Grok")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start interactive chat session (default if no command given)
    Chat {
        /// Optional initial message to send
        #[arg(trailing_var_arg = true)]
        message: Vec<String>,
    },

    /// Manage sessions
    #[command(subcommand)]
    Session(SessionCommands),

    /// Show or set the model
    Model {
        /// Model name to set (omit to show current)
        name: Option<String>,
    },

    /// Clear current session conversation
    Clear,

    /// Compact conversation to reduce token usage
    Compact,

    /// Manage plans
    #[command(subcommand)]
    Plan(PlanCommands),

    /// Show current configuration
    Config,

    /// Manage markdown context files
    #[command(subcommand)]
    Context(ContextCommands),
}

#[derive(Subcommand)]
enum SessionCommands {
    /// List all sessions
    List,
    /// Create a new session
    New,
    /// Resume a previous session by ID
    Resume {
        /// Session ID to resume
        id: String,
    },
    /// Export session as markdown transcript
    Export {
        /// Session ID (defaults to current)
        id: Option<String>,
        /// Output file path (defaults to session-{id}.md)
        #[arg(short, long)]
        output: Option<String>,
    },
}

#[derive(Subcommand)]
enum PlanCommands {
    /// List all saved plans
    List,
    /// Create a new plan
    Create {
        /// Plan name
        name: String,
    },
}

#[derive(Subcommand)]
enum ContextCommands {
    /// Generate environment context snapshot (.bfcode/context/environment.md)
    Env,
    /// Show compaction summary for current session
    Summary,
    /// Save compaction summary as markdown
    Save,
    /// List all context files
    List,
    /// Show combined context that would be injected into system prompt
    Show,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None | Some(Commands::Chat { .. }) => {
            // Extract initial message if provided via `bfcode chat "message"`
            let initial_message = match &cli.command {
                Some(Commands::Chat { message }) if !message.is_empty() => {
                    Some(message.join(" "))
                }
                _ => None,
            };
            run_interactive(initial_message).await
        }
        Some(Commands::Session(cmd)) => run_session_command(cmd),
        Some(Commands::Model { name }) => run_model_command(name),
        Some(Commands::Clear) => run_clear(),
        Some(Commands::Compact) => run_compact(),
        Some(Commands::Plan(cmd)) => run_plan_command(cmd),
        Some(Commands::Config) => run_config(),
        Some(Commands::Context(cmd)) => run_context_command(cmd),
    }
}

// --- Subcommand handlers ---

fn run_session_command(cmd: SessionCommands) -> Result<()> {
    match cmd {
        SessionCommands::List => {
            let sessions = persistence::list_sessions();
            if sessions.is_empty() {
                println!("{}", "No sessions found.".dimmed());
            } else {
                let current = persistence::load_session();
                println!("{}", "Sessions:".yellow().bold());
                for (id, title, updated, msgs) in &sessions {
                    let marker = if *id == current.id { " *" } else { "  " };
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
            Ok(())
        }
        SessionCommands::New => {
            let mut config = persistence::load_config();
            let instructions = persistence::load_instructions();
            let plans_context = persistence::load_plans_context();

            let mut full_system_prompt = config.system_prompt.clone();
            if let Some(ref instr) = instructions {
                full_system_prompt.push_str(instr);
            }
            if let Some(ref plans) = plans_context {
                full_system_prompt.push_str(plans);
            }

            let mut session = persistence::new_session();
            session
                .conversation
                .insert(0, Message::system(&full_system_prompt));
            persistence::save_session(&session)?;
            println!("{}", format!("New session: {}", session.id).green());
            Ok(())
        }
        SessionCommands::Resume { id } => {
            match persistence::switch_session(&id) {
                Some(session) => {
                    println!(
                        "{}",
                        format!("Resumed session: {} ({})", session.id, session.title).green()
                    );
                }
                None => {
                    println!("{}", format!("Session '{id}' not found.").red());
                }
            }
            Ok(())
        }
        SessionCommands::Export { id, output } => {
            let session = match id {
                Some(ref sid) => persistence::switch_session(sid)
                    .ok_or_else(|| anyhow::anyhow!("Session '{sid}' not found"))?,
                None => persistence::load_session(),
            };
            let path = context::export_transcript(&session, output.as_deref())?;
            println!(
                "{}",
                format!("Transcript exported: {}", path.display()).green()
            );
            Ok(())
        }
    }
}

fn run_model_command(name: Option<String>) -> Result<()> {
    let mut config = persistence::load_config();
    match name {
        Some(model) => {
            config.model = model.clone();
            persistence::save_config(&config)?;
            println!("{}", format!("Model set to: {model}").green());
        }
        None => {
            println!("Current model: {}", config.model.cyan());
        }
    }
    Ok(())
}

fn run_clear() -> Result<()> {
    let mut config = persistence::load_config();
    let instructions = persistence::load_instructions();
    let plans_context = persistence::load_plans_context();

    let mut full_system_prompt = config.system_prompt.clone();
    if let Some(ref instr) = instructions {
        full_system_prompt.push_str(instr);
    }
    if let Some(ref plans) = plans_context {
        full_system_prompt.push_str(plans);
    }

    let mut session = persistence::load_session();
    persistence::clear_session(&mut session);
    session
        .conversation
        .insert(0, Message::system(&full_system_prompt));
    persistence::save_session(&session)?;
    println!("{}", "Session cleared.".yellow());
    Ok(())
}

fn run_compact() -> Result<()> {
    let config = persistence::load_config();
    let instructions = persistence::load_instructions();
    let plans_context = persistence::load_plans_context();

    let mut full_system_prompt = config.system_prompt.clone();
    if let Some(ref instr) = instructions {
        full_system_prompt.push_str(instr);
    }
    if let Some(ref plans) = plans_context {
        full_system_prompt.push_str(plans);
    }

    let mut session = persistence::load_session();
    let before = session.conversation.len();
    compact_conversation(&mut session, &full_system_prompt);
    persistence::save_session(&session)?;
    println!(
        "{}",
        format!("Compacted: {before} -> {} messages", session.conversation.len()).yellow()
    );
    Ok(())
}

fn run_plan_command(cmd: PlanCommands) -> Result<()> {
    match cmd {
        PlanCommands::List => {
            let plans = persistence::list_plans();
            if plans.is_empty() {
                println!("{}", "No plans found.".dimmed());
            } else {
                println!("{}", "Plans:".yellow().bold());
                for (name, path) in &plans {
                    println!("  {} {}", name.cyan(), path.dimmed());
                }
            }
            Ok(())
        }
        PlanCommands::Create { name } => {
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
                match persistence::save_plan(&name, &content) {
                    Ok(path) => {
                        println!("{}", format!("Plan saved: {}", path.display()).green());
                    }
                    Err(e) => {
                        println!("{}", format!("Failed to save plan: {e}").red());
                    }
                }
            }
            Ok(())
        }
    }
}

fn run_context_command(cmd: ContextCommands) -> Result<()> {
    match cmd {
        ContextCommands::Env => {
            let path = context::save_environment_context()?;
            println!("{}", format!("Environment context saved: {}", path.display()).green());
            Ok(())
        }
        ContextCommands::Summary => {
            let session = persistence::load_session();
            let summary = context::build_compaction_summary(&session);
            println!("{summary}");
            Ok(())
        }
        ContextCommands::Save => {
            let session = persistence::load_session();
            let (path, _) = context::save_compaction_summary(&session)?;
            println!(
                "{}",
                format!("Compaction summary saved: {}", path.display()).green()
            );
            Ok(())
        }
        ContextCommands::List => {
            let dir = std::path::PathBuf::from(".bfcode").join("context");
            if !dir.exists() {
                println!("{}", "No context files. Run `bfcode context env` to generate.".dimmed());
                return Ok(());
            }
            println!("{}", "Context files:".yellow().bold());
            if let Ok(entries) = std::fs::read_dir(&dir) {
                let mut files: Vec<_> = entries.flatten().collect();
                files.sort_by_key(|e| e.file_name());
                for entry in files {
                    let path = entry.path();
                    if let Ok(meta) = std::fs::metadata(&path) {
                        let size = meta.len();
                        let name = path.file_name().unwrap_or_default().to_string_lossy();
                        println!("  {} ({})", name.cyan(), format_size(size).dimmed());
                    }
                }
            }
            Ok(())
        }
        ContextCommands::Show => {
            match context::load_context_files() {
                Some(ctx) => println!("{ctx}"),
                None => println!("{}", "No context files found.".dimmed()),
            }
            Ok(())
        }
    }
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn run_config() -> Result<()> {
    let config = persistence::load_config();
    println!("{}", "Configuration:".yellow().bold());
    println!("  Model:       {}", config.model.cyan());
    println!("  Temperature: {}", format!("{}", config.temperature).cyan());
    println!(
        "  System prompt: {} chars",
        format!("{}", config.system_prompt.len()).cyan()
    );
    Ok(())
}

// --- Interactive REPL ---

async fn run_interactive(initial_message: Option<String>) -> Result<()> {
    let api_key =
        std::env::var("GROK_API_KEY").context("GROK_API_KEY environment variable not set")?;

    // Load global config
    let mut config = persistence::load_config();

    // Load project session
    let mut session = persistence::load_session();

    // Load project instructions (AGENTS.md, CLAUDE.md, BFCODE.md, etc.)
    let instructions = persistence::load_instructions();

    // Load existing plans as context
    let plans_context = persistence::load_plans_context();

    // Load context markdown files (.bfcode/context/*.md)
    let context_files = context::load_context_files();

    // Build full system prompt = base + instructions + plans + context
    let mut full_system_prompt = config.system_prompt.clone();
    if let Some(ref instr) = instructions {
        full_system_prompt.push_str(instr);
    }
    if let Some(ref plans) = plans_context {
        full_system_prompt.push_str(plans);
    }
    if let Some(ref ctx) = context_files {
        full_system_prompt.push_str(&format!("\n# Context\n{ctx}"));
    }

    // Ensure system prompt is first message
    if session.conversation.is_empty() || session.conversation[0].role != "system" {
        session
            .conversation
            .insert(0, Message::system(&full_system_prompt));
    }

    let client = api::GrokClient::new(api_key)?;
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
    println!("Type {} for commands, or use {} for CLI help", "/help".yellow(), "bfcode --help".yellow());
    println!();

    // If an initial message was provided, process it first
    if let Some(ref msg) = initial_message {
        process_user_message(
            msg,
            &mut session,
            &mut config,
            &full_system_prompt,
            &client,
            &tool_defs,
            &permissions,
        )
        .await?;
    }

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

        process_user_message(
            input,
            &mut session,
            &mut config,
            &full_system_prompt,
            &client,
            &tool_defs,
            &permissions,
        )
        .await?;
    }

    Ok(())
}

async fn process_user_message(
    input: &str,
    session: &mut ProjectSession,
    config: &mut GlobalConfig,
    _full_system_prompt: &str,
    client: &dyn api::ChatClient,
    tool_defs: &[types::ToolDefinition],
    permissions: &tools::Permissions,
) -> Result<()> {
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
                tool_defs,
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
                    permissions,
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

    persistence::save_session(session)?;
    Ok(())
}

// --- Slash command handling (for interactive mode) ---

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
) -> Result<CommandResult> {
    let parts: Vec<&str> = input.splitn(2, ' ').collect();
    let cmd = parts[0];
    let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");

    match cmd {
        "/quit" | "/exit" | "/q" => {
            println!("Goodbye!");
            return Ok(CommandResult::Quit);
        }
        "/help" | "/h" => {
            println!("{}", "Interactive commands:".yellow().bold());
            println!("  {}         - show this help", "/help".yellow());
            println!("  {}        - clear current session", "/clear".yellow());
            println!("  {}      - compact conversation", "/compact".yellow());
            println!("  {}          - create a new session", "/new".yellow());
            println!("  {}     - list all sessions", "/sessions".yellow());
            println!("  {}   - switch to session by ID", "/resume <id>".yellow());
            println!("  {}  - change model", "/model <name>".yellow());
            println!("  {} - save a plan as .md file", "/plan <name>".yellow());
            println!("  {}        - list saved plans", "/plans".yellow());
            println!("  {}       - export session as markdown", "/export".yellow());
            println!("  {}      - show compaction summary", "/context".yellow());
            println!("  {}         - exit", "/quit".yellow());
            println!();
            println!("{}", "CLI commands (from shell):".yellow().bold());
            println!("  {}              - start interactive chat", "bfcode".yellow());
            println!("  {}    - send a message directly", "bfcode chat <msg>".yellow());
            println!("  {} - list/new/resume/export", "bfcode session ...".yellow());
            println!("  {}  - get/set model", "bfcode model [name]".yellow());
            println!("  {}        - clear session", "bfcode clear".yellow());
            println!("  {}      - compact conversation", "bfcode compact".yellow());
            println!("  {}  - list/create plans", "bfcode plan ...".yellow());
            println!("  {} - env/summary/save/list/show", "bfcode context ...".yellow());
            println!("  {}       - show configuration", "bfcode config".yellow());
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
        "/export" => {
            let output = if arg.is_empty() { None } else { Some(arg) };
            match context::export_transcript(session, output) {
                Ok(path) => {
                    println!(
                        "{}",
                        format!("Transcript exported: {}", path.display()).green()
                    );
                }
                Err(e) => {
                    println!("{}", format!("Export failed: {e}").red());
                }
            }
        }
        "/context" => {
            let summary = context::build_compaction_summary(session);
            println!("{}", "Compaction summary:".yellow().bold());
            println!("{summary}");
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

pub(crate) fn format_tokens(tokens: u64) -> String {
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

    // Build structured summary before compacting
    let summary = context::build_compaction_summary(session);

    // Also persist the summary as a markdown file
    let _ = context::save_compaction_summary(session);

    let mut compacted = Vec::new();

    compacted.push(Message::system(full_system_prompt));

    let total = messages.len();
    compacted.push(Message::system(&format!(
        "[Previous conversation compacted. {total} messages summarized.]\n\n{summary}"
    )));

    let keep_from = messages.len().saturating_sub(8);
    for msg in &messages[keep_from..] {
        if msg.role != "system" {
            compacted.push(msg.clone());
        }
    }

    session.conversation = compacted;
}

// --- Tests: agent loop with MockClient ---

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::MockClient;
    use std::sync::Mutex;

    static CWD_LOCK: Mutex<()> = Mutex::new(());

    /// Run a closure in a temp dir, holding the cwd lock
    fn with_tmp<F, R>(f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let _lock = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!(
            "bfcode_main_test_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(&tmp).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        std::env::set_current_dir(&original).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
        match result {
            Ok(r) => r,
            Err(e) => std::panic::resume_unwind(e),
        }
    }

    /// Helper: create a minimal session
    fn new_test_session() -> ProjectSession {
        let mut session = ProjectSession::new();
        session
            .conversation
            .push(Message::system("You are a test assistant."));
        session
    }

    /// Helper: run async code inside with_tmp
    async fn run_in_tmp<F, Fut>(f: F)
    where
        F: FnOnce() -> Fut + Send,
        Fut: std::future::Future<Output = ()>,
    {
        // We need to set cwd before the async work, so use a sync lock
        let _lock = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!(
            "bfcode_main_test_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(&tmp).unwrap();

        f().await;

        std::env::set_current_dir(&original).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── Simple text response ──────────────────────────────────────────

    #[tokio::test]
    async fn test_agent_loop_text_response() {
        run_in_tmp(|| async {
            let mock = MockClient::new(vec![MockClient::text_response(
                "Hello! How can I help?",
            )]);
            let tool_defs = tools::get_tool_definitions();
            let permissions = tools::Permissions::new();
            let mut session = new_test_session();
            let mut config = GlobalConfig::default();

            process_user_message(
                "hi there", &mut session, &mut config, "sys",
                &mock, &tool_defs, &permissions,
            )
            .await
            .unwrap();

            assert_eq!(session.conversation.len(), 3);
            assert_eq!(session.conversation[1].role, "user");
            assert_eq!(session.conversation[1].content.as_deref(), Some("hi there"));
            assert_eq!(session.conversation[2].role, "assistant");
            assert_eq!(session.conversation[2].content.as_deref(), Some("Hello! How can I help?"));
            assert_eq!(session.total_tokens, 15);
            assert_eq!(session.title, "hi there");
        })
        .await;
    }

    // ── Tool call → tool result → final text ─────────────────────────

    #[tokio::test]
    async fn test_agent_loop_tool_call_then_text() {
        run_in_tmp(|| async {
            let mock = MockClient::new(vec![
                MockClient::tool_call_response(vec![(
                    "call_1".into(), "list_files".into(), r#"{"path":"."}"#.into(),
                )]),
                MockClient::text_response("I see the project files."),
            ]);
            let tool_defs = tools::get_tool_definitions();
            let permissions = tools::Permissions::new();
            let mut session = new_test_session();
            let mut config = GlobalConfig::default();

            process_user_message(
                "what files are here?", &mut session, &mut config, "sys",
                &mock, &tool_defs, &permissions,
            )
            .await
            .unwrap();

            assert_eq!(session.conversation.len(), 5);
            assert_eq!(session.conversation[2].role, "assistant");
            assert!(session.conversation[2].tool_calls.is_some());
            assert_eq!(session.conversation[3].role, "tool");
            assert_eq!(session.conversation[3].tool_call_id.as_deref(), Some("call_1"));
            assert_eq!(session.conversation[4].role, "assistant");
            assert_eq!(session.conversation[4].content.as_deref(), Some("I see the project files."));
            assert_eq!(mock.requests().len(), 2);
            assert!(mock.requests()[1].messages.iter().any(|m| m.role == "tool"));
        })
        .await;
    }

    // ── Multiple tool calls in one response ──────────────────────────

    #[tokio::test]
    async fn test_agent_loop_multiple_tool_calls() {
        run_in_tmp(|| async {
            let mock = MockClient::new(vec![
                MockClient::tool_call_response(vec![
                    ("c1".into(), "list_files".into(), r#"{"path":"."}"#.into()),
                    ("c2".into(), "list_files".into(), r#"{"path":"."}"#.into()),
                ]),
                MockClient::text_response("Found the files."),
            ]);
            let tool_defs = tools::get_tool_definitions();
            let permissions = tools::Permissions::new();
            let mut session = new_test_session();
            let mut config = GlobalConfig::default();

            process_user_message(
                "list root and src", &mut session, &mut config, "sys",
                &mock, &tool_defs, &permissions,
            )
            .await
            .unwrap();

            assert_eq!(session.conversation.len(), 6);
            let tool_results: Vec<_> = session.conversation.iter().filter(|m| m.role == "tool").collect();
            assert_eq!(tool_results.len(), 2);
        })
        .await;
    }

    // ── Chained tool calls (multi-round) ─────────────────────────────

    #[tokio::test]
    async fn test_agent_loop_chained_tool_calls() {
        run_in_tmp(|| async {
            let mock = MockClient::new(vec![
                MockClient::tool_call_response(vec![(
                    "c1".into(), "list_files".into(), r#"{"path":"."}"#.into(),
                )]),
                MockClient::tool_call_response(vec![(
                    "c2".into(), "list_files".into(), r#"{"path":"."}"#.into(),
                )]),
                MockClient::text_response("All done with both lookups."),
            ]);
            let tool_defs = tools::get_tool_definitions();
            let permissions = tools::Permissions::new();
            let mut session = new_test_session();
            let mut config = GlobalConfig::default();

            let _ = process_user_message(
                "explore the project", &mut session, &mut config, "sys",
                &mock, &tool_defs, &permissions,
            )
            .await;

            assert_eq!(mock.requests().len(), 3);
            let last = session.conversation.last().unwrap();
            assert_eq!(last.role, "assistant");
            assert_eq!(last.content.as_deref(), Some("All done with both lookups."));
            assert_eq!(session.total_tokens, 30 + 30 + 15);
        })
        .await;
    }

    // ── API error handling ───────────────────────────────────────────

    #[tokio::test]
    async fn test_agent_loop_api_error_removes_user_message() {
        run_in_tmp(|| async {
            let mock = MockClient::with_error("connection refused");
            let tool_defs = tools::get_tool_definitions();
            let permissions = tools::Permissions::new();
            let mut session = new_test_session();
            let mut config = GlobalConfig::default();

            let result = process_user_message(
                "this will fail", &mut session, &mut config, "sys",
                &mock, &tool_defs, &permissions,
            )
            .await;

            assert!(result.is_ok());
            // User message removed on error, only system remains
            assert_eq!(session.conversation.len(), 1);
            assert_eq!(session.conversation[0].role, "system");
        })
        .await;
    }

    // ── Mock captures correct request data ───────────────────────────

    #[tokio::test]
    async fn test_mock_captures_request_data() {
        run_in_tmp(|| async {
            let mock = MockClient::new(vec![MockClient::text_response("ok")]);
            let tool_defs = tools::get_tool_definitions();
            let permissions = tools::Permissions::new();
            let mut session = new_test_session();
            let mut config = GlobalConfig::default();
            config.model = "test-model-v1".into();
            config.temperature = 0.7;

            process_user_message(
                "test input", &mut session, &mut config, "sys",
                &mock, &tool_defs, &permissions,
            )
            .await
            .unwrap();

            let reqs = mock.requests();
            assert_eq!(reqs.len(), 1);
            assert_eq!(reqs[0].model, "test-model-v1");
            assert_eq!(reqs[0].temperature, 0.7);
            assert!(reqs[0].messages.iter().any(|m| m.role == "system"));
            assert!(reqs[0].messages.iter().any(|m| m.role == "user"));
        })
        .await;
    }

    // ── Session title auto-set ───────────────────────────────────────

    #[tokio::test]
    async fn test_session_title_auto_set() {
        run_in_tmp(|| async {
            let mock = MockClient::new(vec![MockClient::text_response("hi")]);
            let tool_defs = tools::get_tool_definitions();
            let permissions = tools::Permissions::new();
            let mut session = new_test_session();
            let mut config = GlobalConfig::default();

            assert_eq!(session.title, "New session");

            process_user_message(
                "refactor the auth module to use JWT", &mut session, &mut config, "sys",
                &mock, &tool_defs, &permissions,
            )
            .await
            .unwrap();

            assert_eq!(session.title, "refactor the auth module to use JWT");
        })
        .await;
    }

    #[tokio::test]
    async fn test_session_title_truncated_at_50_chars() {
        run_in_tmp(|| async {
            let mock = MockClient::new(vec![MockClient::text_response("ok")]);
            let tool_defs = tools::get_tool_definitions();
            let permissions = tools::Permissions::new();
            let mut session = new_test_session();
            let mut config = GlobalConfig::default();

            let long_msg = "a".repeat(100);
            process_user_message(
                &long_msg, &mut session, &mut config, "sys",
                &mock, &tool_defs, &permissions,
            )
            .await
            .unwrap();

            assert_eq!(session.title.len(), 50);
        })
        .await;
    }

    // ── Multiple user turns ──────────────────────────────────────────

    #[tokio::test]
    async fn test_multi_turn_conversation() {
        run_in_tmp(|| async {
            let mock = MockClient::new(vec![
                MockClient::text_response("Hello!"),
                MockClient::text_response("I can help with that."),
            ]);
            let tool_defs = tools::get_tool_definitions();
            let permissions = tools::Permissions::new();
            let mut session = new_test_session();
            let mut config = GlobalConfig::default();

            process_user_message(
                "hi", &mut session, &mut config, "sys",
                &mock, &tool_defs, &permissions,
            )
            .await
            .unwrap();

            process_user_message(
                "help me refactor", &mut session, &mut config, "sys",
                &mock, &tool_defs, &permissions,
            )
            .await
            .unwrap();

            assert_eq!(session.conversation.len(), 5);
            assert_eq!(session.total_tokens, 30);
            let reqs = mock.requests();
            assert_eq!(reqs.len(), 2);
            // Second call has full history: system + user1 + assistant1 + user2
            assert_eq!(reqs[1].messages.len(), 4);
        })
        .await;
    }

    // ── Tool call with read (no permissions needed) ──────────────────

    #[tokio::test]
    async fn test_agent_loop_read_tool_executes() {
        run_in_tmp(|| async {
            std::fs::write("hello.txt", "world content here").unwrap();

            let mock = MockClient::new(vec![
                MockClient::tool_call_response(vec![(
                    "c1".into(), "read".into(),
                    r#"{"path":"hello.txt"}"#.into(),
                )]),
                MockClient::text_response("The file contains world content."),
            ]);
            let tool_defs = tools::get_tool_definitions();
            let permissions = tools::Permissions::new();
            let mut session = new_test_session();
            let mut config = GlobalConfig::default();

            process_user_message(
                "read hello.txt", &mut session, &mut config, "sys",
                &mock, &tool_defs, &permissions,
            )
            .await
            .unwrap();

            let tool_msg = session.conversation.iter().find(|m| m.role == "tool").unwrap();
            let content = tool_msg.content.as_deref().unwrap();
            assert!(content.contains("world content here"), "Read should return file content: {content}");
        })
        .await;
    }

    // ── Empty response from API ──────────────────────────────────────

    #[tokio::test]
    async fn test_agent_loop_empty_choices() {
        run_in_tmp(|| async {
            let mock = MockClient::new(vec![ChatResponse {
                choices: vec![],
                usage: Some(Usage {
                    prompt_tokens: 5,
                    completion_tokens: 0,
                    total_tokens: 5,
                }),
            }]);
            let tool_defs = tools::get_tool_definitions();
            let permissions = tools::Permissions::new();
            let mut session = new_test_session();
            let mut config = GlobalConfig::default();

            let result = process_user_message(
                "test", &mut session, &mut config, "sys",
                &mock, &tool_defs, &permissions,
            )
            .await;

            assert!(result.is_ok());
            assert_eq!(session.conversation.len(), 1);
        })
        .await;
    }

    // ── format_tokens helper ─────────────────────────────────────────

    #[test]
    fn test_format_tokens_units() {
        assert_eq!(format_tokens(500), "500");
        assert_eq!(format_tokens(1_500), "1.5K");
        assert_eq!(format_tokens(1_500_000), "1.5M");
    }

    // ── Compact with structured summary ──────────────────────────────

    #[test]
    fn test_compact_conversation_uses_structured_summary() {
        with_tmp(|| {
            let mut session = ProjectSession::new();
            session.conversation.push(Message::system("You are helpful."));
            session.conversation.push(Message::user("fix the bug"));
            session.conversation.push(Message::assistant_text("Looking at it."));

            for i in 0..12 {
                session.conversation.push(Message::user(&format!("step {i}")));
                session.conversation.push(Message::assistant_text(&format!("done {i}")));
            }

            compact_conversation(&mut session, "You are helpful.");

            let summary_msg = &session.conversation[1];
            assert_eq!(summary_msg.role, "system");
            let content = summary_msg.content.as_deref().unwrap();
            assert!(content.contains("## Goal"));
            assert!(content.contains("## Accomplished"));
            assert!(content.contains("fix the bug"));
        });
    }

    use crate::types::{ChatResponse, Usage};
}
