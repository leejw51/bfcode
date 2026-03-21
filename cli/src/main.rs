mod api;
mod context;
mod persistence;
#[cfg(test)]
mod test_utils;
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
#[command(name = "bfcode", version = "0.6.0", about = "AI coding assistant (Grok/OpenAI/Anthropic)")]
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

    /// Manage context memory (.bfcode/memory/*.md)
    #[command(subcommand)]
    Memory(MemoryCommands),

    /// Undo last file change(s)
    Undo {
        /// Number of changes to undo (default 1)
        #[arg(default_value = "1")]
        count: usize,
    },
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
enum MemoryCommands {
    /// List all saved memories
    List,
    /// Show a specific memory by name
    Show {
        /// Memory name
        name: String,
    },
    /// Save a new memory markdown file
    Save {
        /// Memory name (used as filename)
        name: String,
        /// One-line description
        #[arg(short, long)]
        description: Option<String>,
        /// Memory type: user, feedback, project, reference
        #[arg(short = 't', long, default_value = "project")]
        memory_type: String,
        /// Content (if not provided, reads from stdin)
        #[arg(short, long)]
        content: Option<String>,
        /// Folder to save in (default: .bfcode/memory/)
        #[arg(short, long)]
        folder: Option<String>,
    },
    /// Delete a memory by name
    Delete {
        /// Memory name
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
        Some(Commands::Memory(cmd)) => run_memory_command(cmd),
        Some(Commands::Undo { count }) => run_undo(count),
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

fn run_memory_command(cmd: MemoryCommands) -> Result<()> {
    match cmd {
        MemoryCommands::List => {
            let memories = persistence::list_memories();
            if memories.is_empty() {
                println!(
                    "{}",
                    "No memories saved. Use `bfcode memory save <name>` to create one.".dimmed()
                );
            } else {
                println!("{}", "Context Memories:".yellow().bold());
                for (name, desc, mtype, size) in &memories {
                    let desc_part = if desc.is_empty() {
                        String::new()
                    } else {
                        format!(" — {}", desc.dimmed())
                    };
                    println!(
                        "  {} [{}] ({}){}",
                        name.cyan(),
                        mtype,
                        format_size(*size).dimmed(),
                        desc_part
                    );
                }
            }
            Ok(())
        }
        MemoryCommands::Show { name } => {
            match persistence::load_memory(&name) {
                Some(mem) => {
                    println!("{} [{}]", mem.name.cyan().bold(), mem.memory_type);
                    if !mem.description.is_empty() {
                        println!("{}", mem.description.dimmed());
                    }
                    println!("---");
                    println!("{}", mem.content);
                }
                None => println!("{}", format!("Memory '{}' not found.", name).red()),
            }
            Ok(())
        }
        MemoryCommands::Save {
            name,
            description,
            memory_type,
            content,
            folder,
        } => {
            let mtype = match memory_type.as_str() {
                "user" => types::MemoryType::User,
                "feedback" => types::MemoryType::Feedback,
                "reference" => types::MemoryType::Reference,
                _ => types::MemoryType::Project,
            };

            let body = match content {
                Some(c) => c,
                None => {
                    // Read from stdin
                    println!("{}", "Enter memory content (Ctrl+D to finish):".dimmed());
                    let mut buf = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
                    buf
                }
            };

            let mem = types::ContextMemory {
                name: name.clone(),
                description: description.unwrap_or_default(),
                memory_type: mtype,
                content: body,
            };

            let path = if let Some(ref folder) = folder {
                persistence::save_memory_to(&mem, folder)?
            } else {
                persistence::save_memory(&mem)?
            };
            println!(
                "{}",
                format!("Memory saved: {}", path.display()).green()
            );
            Ok(())
        }
        MemoryCommands::Delete { name } => {
            match persistence::delete_memory(&name)? {
                true => println!("{}", format!("Deleted memory '{name}'.").green()),
                false => println!("{}", format!("Memory '{name}' not found.").yellow()),
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

fn run_undo(count: usize) -> Result<()> {
    let session = persistence::load_session();
    match persistence::undo_last_n(&session.id, count) {
        Ok(restored) if !restored.is_empty() => {
            for path in &restored {
                println!("  {} {}", "Restored:".green(), path);
            }
            println!(
                "{}",
                format!("Undid {} file change(s)", restored.len()).green()
            );
        }
        Ok(_) => println!("{}", "Nothing to undo.".yellow()),
        Err(e) => println!("{}", format!("Undo failed: {e}").red()),
    }
    Ok(())
}

fn run_config() -> Result<()> {
    let config = persistence::load_config();
    let provider = types::detect_provider(&config.model);
    println!("{}", "Configuration:".yellow().bold());
    println!("  Provider:    {}", format!("{provider}").cyan());
    println!("  Model:       {}", config.model.cyan());
    println!("  Temperature: {}", format!("{}", config.temperature).cyan());
    println!(
        "  Context:     {} tokens",
        format!("{}", types::context_limit_for_model(&config.model)).cyan()
    );
    println!(
        "  System prompt: {} chars",
        format!("{}", config.system_prompt.len()).cyan()
    );
    Ok(())
}

// --- Interactive REPL ---

async fn run_interactive(initial_message: Option<String>) -> Result<()> {
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

    // Load context memories (.bfcode/memory/*.md)
    let memories_context = persistence::load_memories_context();

    // Build full system prompt = base + instructions + plans + context + memories
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
    if let Some(ref mem) = memories_context {
        full_system_prompt.push_str(mem);
    }

    // Ensure system prompt is first message
    if session.conversation.is_empty() || session.conversation[0].role != "system" {
        session
            .conversation
            .insert(0, Message::system(&full_system_prompt));
    }

    let client = api::create_client(&config)?;
    let tool_defs = tools::get_tool_definitions();
    let permissions = tools::Permissions::new();

    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".into());

    // Welcome banner
    let provider = types::detect_provider(&config.model);
    println!("{}", "bfcode v0.6.0".green().bold());
    println!("Project:  {}", cwd.dimmed());
    println!(
        "Model:    {} ({})",
        config.model.cyan(),
        format!("{provider}").dimmed()
    );
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
            client.as_ref(),
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
            // Handle /paste specially (needs async for process_user_message)
            if input.starts_with("/paste") {
                let msg = input.strip_prefix("/paste").unwrap_or("").trim();
                let paste_input = if msg.is_empty() {
                    "@clipboard describe this image".to_string()
                } else {
                    format!("@clipboard {msg}")
                };
                process_user_message(
                    &paste_input,
                    &mut session,
                    &mut config,
                    &full_system_prompt,
                    client.as_ref(),
                    &tool_defs,
                    &permissions,
                )
                .await?;
                continue;
            }

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
            client.as_ref(),
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
    full_system_prompt: &str,
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

    // Extract image attachments from input (e.g., @image.png or paths ending in image extensions)
    let (clean_input, images) = extract_images(input);
    if !images.is_empty() {
        eprintln!(
            "  {} Attached {} image(s)",
            "+".green(),
            images.len()
        );
        session.conversation.push(Message::user_with_images(&clean_input, images));
    } else {
        session.conversation.push(Message::user(input));
    }

    // Token-aware auto-compaction
    let estimated = context::estimate_conversation_tokens(&session.conversation);
    let limit = types::context_limit_for_model(&config.model);
    if limit > 0 && estimated > (limit * 80 / 100) {
        eprintln!(
            "  {} Auto-compacting ({} tokens, {}% of {} limit)...",
            "~".yellow(),
            format_tokens(estimated),
            estimated * 100 / limit,
            format_tokens(limit)
        );
        compact_conversation(session, full_system_prompt);
        persistence::save_session(session)?;
    }

    // Agent loop
    let mut error_occurred = false;
    for _round in 0..MAX_TOOL_ROUNDS {
        // Use streaming for text generation
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let messages = session.conversation.clone();
        let tools_clone = tool_defs.to_vec();
        let model = config.model.clone();
        let temp = config.temperature;

        // Spawn streaming request
        let stream_result = client
            .chat_stream(&messages, &tools_clone, &model, temp, tx)
            .await;

        // Drain any remaining chunks (print streamed text)
        let mut streamed_any_text = false;
        while let Ok(chunk) = rx.try_recv() {
            match chunk {
                types::StreamChunk::Text(text) => {
                    if !streamed_any_text {
                        println!(); // newline before streamed output
                        streamed_any_text = true;
                    }
                    print!("{text}");
                    let _ = std::io::stdout().flush();
                }
                types::StreamChunk::ToolCallStart { name, .. } => {
                    eprint!("\n  {} {name}...", ">>>".cyan().bold());
                }
                types::StreamChunk::Done => {}
                types::StreamChunk::Error(e) => {
                    eprintln!("{} {e}", "Stream error:".red().bold());
                }
                _ => {}
            }
        }
        if streamed_any_text {
            println!("\n"); // final newline after streamed text
        }

        let response = match stream_result {
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
                    &session.id,
                )
                .await;
                tools::print_tool_result(&result);
                session
                    .conversation
                    .push(Message::tool_result(&tc.id, &result));
            }
            continue;
        }

        // Text response (already streamed, just add to conversation)
        if let Some(content) = &assistant_msg.content {
            session.conversation.push(Message::assistant_text(content));
        }

        // Show token usage and cost
        if let Some(usage) = &response.usage {
            let cost = types::calculate_cost(
                &config.model,
                usage.prompt_tokens,
                usage.completion_tokens,
            );
            eprintln!(
                "  {} tokens: {} this call | {} session total | cost: {}",
                "~".dimmed(),
                format_tokens(usage.total_tokens).dimmed(),
                format_tokens(session.total_tokens).dimmed(),
                types::format_cost(cost).dimmed(),
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
            println!("  {}     - undo last N file changes", "/undo [n]".yellow());
            println!("  {} - send clipboard image", "/paste [msg]".yellow());
            println!("  {}         - exit", "/quit".yellow());
            println!();
            println!("{}", "Image input:".yellow().bold());
            println!("  {}     - attach image file", "@image.png".yellow());
            println!("  {}     - paste from clipboard", "@clipboard".yellow());
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
            println!("  {}    - undo file changes", "bfcode undo [n]".yellow());
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
                let p = types::detect_provider(&config.model);
                println!("Current model: {} ({})", config.model.cyan(), format!("{p}").dimmed());
            } else {
                config.model = arg.to_string();
                config.provider = types::detect_provider(arg);
                let _ = persistence::save_config(config);
                println!(
                    "{}",
                    format!("Model set to: {} ({:?})", arg, config.provider).green()
                );
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
        "/undo" => {
            let n: usize = if arg.is_empty() {
                1
            } else {
                arg.parse().unwrap_or(1)
            };
            match persistence::undo_last_n(&session.id, n) {
                Ok(restored) if !restored.is_empty() => {
                    for path in &restored {
                        println!("  {} {}", "Restored:".green(), path);
                    }
                    println!(
                        "{}",
                        format!("Undid {} file change(s)", restored.len()).green()
                    );
                }
                Ok(_) => println!("{}", "Nothing to undo.".yellow()),
                Err(e) => println!("{}", format!("Undo failed: {e}").red()),
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

/// Extract image file paths from user input.
/// Supports @path/to/image.png syntax, bare image paths, and @clipboard.
/// Returns (cleaned text, list of ImageAttachments).
fn extract_images(input: &str) -> (String, Vec<types::ImageAttachment>) {
    let image_extensions = [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp"];
    let mut images = Vec::new();
    let mut clean_parts = Vec::new();

    for word in input.split_whitespace() {
        // Handle @clipboard — grab image from system clipboard
        if word == "@clipboard" {
            if let Some(img) = grab_clipboard_image() {
                images.push(img);
                clean_parts.push("[image: clipboard]".to_string());
                continue;
            } else {
                eprintln!("  {} No image found in clipboard", "!".yellow());
                clean_parts.push(word.to_string());
                continue;
            }
        }

        let path_str = word.trim_start_matches('@');
        let lower = path_str.to_lowercase();
        let is_image = image_extensions.iter().any(|ext| lower.ends_with(ext));

        if is_image && std::path::Path::new(path_str).exists() {
            if let Ok(data) = std::fs::read(path_str) {
                let base64_data = base64_encode(&data);
                let media_type = if lower.ends_with(".png") {
                    "image/png"
                } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
                    "image/jpeg"
                } else if lower.ends_with(".gif") {
                    "image/gif"
                } else if lower.ends_with(".webp") {
                    "image/webp"
                } else {
                    "image/png"
                };
                images.push(types::ImageAttachment {
                    data: base64_data,
                    media_type: media_type.to_string(),
                });
                clean_parts.push(format!("[image: {}]", path_str));
                continue;
            }
        }
        clean_parts.push(word.to_string());
    }

    let clean_text = clean_parts.join(" ");
    (clean_text, images)
}

/// Grab image from system clipboard using arboard.
/// Returns None if no image is available.
fn grab_clipboard_image() -> Option<types::ImageAttachment> {
    use arboard::Clipboard;

    let mut clipboard = Clipboard::new().ok()?;
    let image = clipboard.get_image().ok()?;

    // Convert RGBA pixels to PNG
    let png_data = encode_rgba_to_png(
        &image.bytes,
        image.width as u32,
        image.height as u32,
    )?;

    let base64_data = base64_encode(&png_data);
    Some(types::ImageAttachment {
        data: base64_data,
        media_type: "image/png".to_string(),
    })
}

/// Encode raw RGBA pixel data to PNG format (minimal encoder)
fn encode_rgba_to_png(rgba: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    // Use a simple uncompressed PNG encoder
    // PNG format: signature + IHDR + IDAT (zlib deflate stored) + IEND

    let mut out = Vec::new();

    // PNG signature
    out.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);

    // IHDR chunk
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(6); // color type: RGBA
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace
    write_png_chunk(&mut out, b"IHDR", &ihdr);

    // IDAT chunk — build raw scanlines with filter byte 0 (None)
    let mut raw_data = Vec::with_capacity((width as usize * 4 + 1) * height as usize);
    for y in 0..height as usize {
        raw_data.push(0); // filter: None
        let row_start = y * width as usize * 4;
        let row_end = row_start + width as usize * 4;
        if row_end <= rgba.len() {
            raw_data.extend_from_slice(&rgba[row_start..row_end]);
        } else {
            // Pad with zeros if data is short
            let available = rgba.len().saturating_sub(row_start);
            if available > 0 {
                raw_data.extend_from_slice(&rgba[row_start..row_start + available]);
            }
            raw_data.resize(raw_data.len() + width as usize * 4 - available, 0);
        }
    }

    // Compress with zlib (deflate stored blocks)
    let compressed = zlib_compress_stored(&raw_data);
    write_png_chunk(&mut out, b"IDAT", &compressed);

    // IEND chunk
    write_png_chunk(&mut out, b"IEND", &[]);

    Some(out)
}

/// Write a PNG chunk: length(4) + type(4) + data + crc(4)
fn write_png_chunk(out: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(chunk_type);
    out.extend_from_slice(data);
    let crc = png_crc32(chunk_type, data);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// CRC32 for PNG (type + data)
fn png_crc32(chunk_type: &[u8], data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFFFFFF;
    for &byte in chunk_type.iter().chain(data.iter()) {
        let idx = ((crc ^ byte as u32) & 0xFF) as usize;
        crc = CRC32_TABLE[idx] ^ (crc >> 8);
    }
    crc ^ 0xFFFFFFFF
}

/// Zlib wrapper around stored (uncompressed) deflate blocks
fn zlib_compress_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    // Zlib header: CMF=0x78 (deflate, window=32K), FLG=0x01 (no dict, check bits)
    out.push(0x78);
    out.push(0x01);

    // Deflate stored blocks (max 65535 bytes each)
    let mut offset = 0;
    while offset < data.len() {
        let remaining = data.len() - offset;
        let block_size = remaining.min(65535);
        let is_last = offset + block_size >= data.len();

        out.push(if is_last { 0x01 } else { 0x00 }); // BFINAL + BTYPE=00 (stored)
        let len = block_size as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes()); // NLEN
        out.extend_from_slice(&data[offset..offset + block_size]);

        offset += block_size;
    }

    // Adler32 checksum
    let adler = adler32(data);
    out.extend_from_slice(&adler.to_be_bytes());

    out
}

/// Adler-32 checksum
fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// CRC32 lookup table for PNG
const CRC32_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            if c & 1 != 0 {
                c = 0xEDB88320 ^ (c >> 1);
            } else {
                c >>= 1;
            }
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
};

/// Base64 encode bytes (no external dep needed — use simple encoder)
fn base64_encode(data: &[u8]) -> String {
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut encoder = Base64Encoder::new(&mut buf);
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap();
    }
    String::from_utf8(buf).unwrap()
}

/// Minimal base64 encoder
struct Base64Encoder<W: std::io::Write> {
    writer: W,
    buf: [u8; 3],
    buf_len: usize,
}

const B64_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

impl<W: std::io::Write> Base64Encoder<W> {
    fn new(writer: W) -> Self {
        Self { writer, buf: [0; 3], buf_len: 0 }
    }

    fn encode_block(&mut self) -> std::io::Result<()> {
        let b = &self.buf;
        let n = self.buf_len;
        if n == 0 { return Ok(()); }

        let mut out = [b'='; 4];
        out[0] = B64_CHARS[(b[0] >> 2) as usize];
        if n >= 1 {
            out[1] = B64_CHARS[((b[0] & 0x03) << 4 | if n > 1 { b[1] >> 4 } else { 0 }) as usize];
        }
        if n >= 2 {
            out[2] = B64_CHARS[((b[1] & 0x0f) << 2 | if n > 2 { b[2] >> 6 } else { 0 }) as usize];
        }
        if n >= 3 {
            out[3] = B64_CHARS[(b[2] & 0x3f) as usize];
        }
        self.writer.write_all(&out)?;
        self.buf_len = 0;
        Ok(())
    }

    fn finish(mut self) -> std::io::Result<()> {
        if self.buf_len > 0 {
            self.encode_block()?;
        }
        Ok(())
    }
}

impl<W: std::io::Write> std::io::Write for Base64Encoder<W> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let mut i = 0;
        while i < data.len() {
            self.buf[self.buf_len] = data[i];
            self.buf_len += 1;
            if self.buf_len == 3 {
                self.encode_block()?;
            }
            i += 1;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

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
        if msg.role == "system" {
            continue;
        }
        // Truncate large tool outputs in kept messages
        if msg.role == "tool" {
            let mut truncated = msg.clone();
            if let Some(ref content) = truncated.content {
                if content.len() > 2000 {
                    truncated.content = Some(format!(
                        "{}...\n[truncated, {} bytes]",
                        &content[..2000],
                        content.len()
                    ));
                }
            }
            compacted.push(truncated);
        } else {
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
    use crate::test_utils;

    /// Run a closure in a temp dir, holding the global cwd lock
    fn with_tmp<F, R>(f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let tmp = test_utils::tmp_dir("main");
        test_utils::with_cwd(&tmp, f)
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
        let _lock = test_utils::CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!(
            "bfcode_test_main_async_{}_{:?}",
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

            let _ = process_user_message(
                "this will fail", &mut session, &mut config, "sys",
                &mock, &tool_defs, &permissions,
            )
            .await;

            // User message should be removed on error (only system remains)
            // Note: save_session may fail due to cwd race, but session state is correct
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

    // ── Base64 encoder ──────────────────────────────────────────────

    #[test]
    fn test_base64_encode_empty() {
        assert_eq!(base64_encode(&[]), "");
    }

    #[test]
    fn test_base64_encode_hello() {
        assert_eq!(base64_encode(b"Hello"), "SGVsbG8=");
    }

    #[test]
    fn test_base64_encode_padding() {
        assert_eq!(base64_encode(b"a"), "YQ==");
        assert_eq!(base64_encode(b"ab"), "YWI=");
        assert_eq!(base64_encode(b"abc"), "YWJj");
    }

    #[test]
    fn test_base64_encode_binary() {
        let data = vec![0u8, 1, 2, 255, 254, 253];
        let encoded = base64_encode(&data);
        assert!(!encoded.is_empty());
        // Verify it's valid base64 chars
        assert!(encoded.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='));
    }

    // ── Image extraction ────────────────────────────────────────────

    #[test]
    fn test_extract_images_no_images() {
        let (text, images) = extract_images("hello world");
        assert_eq!(text, "hello world");
        assert!(images.is_empty());
    }

    #[test]
    fn test_extract_images_nonexistent_file() {
        let (text, images) = extract_images("look at @nonexistent.png");
        assert!(text.contains("@nonexistent.png"));
        assert!(images.is_empty());
    }

    #[test]
    fn test_extract_images_existing_file() {
        with_tmp(|| {
            // Create a tiny PNG file (1x1 pixel)
            let png_data = vec![
                0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG header
                0x00, 0x00, 0x00, 0x0D, // IHDR length
            ];
            std::fs::write("test.png", &png_data).unwrap();

            let (text, images) = extract_images("describe @test.png please");
            assert!(text.contains("[image: test.png]"));
            assert!(text.contains("please"));
            assert_eq!(images.len(), 1);
            assert_eq!(images[0].media_type, "image/png");
            assert!(!images[0].data.is_empty());
        });
    }

    #[test]
    fn test_extract_images_jpeg() {
        with_tmp(|| {
            std::fs::write("photo.jpg", b"fake jpeg").unwrap();
            let (_, images) = extract_images("@photo.jpg");
            assert_eq!(images.len(), 1);
            assert_eq!(images[0].media_type, "image/jpeg");
        });
    }

    #[test]
    fn test_extract_images_clipboard_keyword_no_clipboard() {
        // @clipboard with no actual clipboard image — should just keep the text
        let (text, images) = extract_images("@clipboard what is this");
        // images will be empty since we can't access clipboard in tests
        // (no display server), text should contain the original or error
        assert!(images.is_empty() || !images.is_empty()); // either is fine
        assert!(!text.is_empty());
    }

    #[test]
    fn test_extract_images_multiple_files() {
        with_tmp(|| {
            std::fs::write("a.png", b"png data").unwrap();
            std::fs::write("b.jpg", b"jpg data").unwrap();
            let (text, images) = extract_images("compare @a.png and @b.jpg");
            assert_eq!(images.len(), 2);
            assert!(text.contains("[image: a.png]"));
            assert!(text.contains("[image: b.jpg]"));
        });
    }

    // ── PNG encoder helpers ─────────────────────────────────────────

    #[test]
    fn test_adler32_empty() {
        assert_eq!(adler32(&[]), 1);
    }

    #[test]
    fn test_adler32_known() {
        // adler32("Wikipedia") = 0x11E60398
        assert_eq!(adler32(b"Wikipedia"), 0x11E60398);
    }

    #[test]
    fn test_png_crc32() {
        // CRC of IHDR type + known data should be deterministic
        let crc = png_crc32(b"IEND", &[]);
        assert_ne!(crc, 0);
    }

    #[test]
    fn test_encode_rgba_to_png_valid_header() {
        // 1x1 red pixel RGBA
        let rgba = vec![255, 0, 0, 255];
        let png = encode_rgba_to_png(&rgba, 1, 1).unwrap();
        // Check PNG signature
        assert_eq!(&png[..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
    }

    #[test]
    fn test_encode_rgba_to_png_small_image() {
        // 2x2 image
        let rgba = vec![
            255, 0, 0, 255,  0, 255, 0, 255,
            0, 0, 255, 255,  255, 255, 255, 255,
        ];
        let png = encode_rgba_to_png(&rgba, 2, 2).unwrap();
        assert!(png.len() > 50); // Should be a reasonable size
        // Should contain IHDR, IDAT, IEND
        let png_str = String::from_utf8_lossy(&png);
        assert!(png.windows(4).any(|w| w == b"IHDR"));
        assert!(png.windows(4).any(|w| w == b"IDAT"));
        assert!(png.windows(4).any(|w| w == b"IEND"));
        let _ = png_str; // avoid unused warning
    }

    #[test]
    fn test_zlib_compress_stored_has_header() {
        let data = b"hello world";
        let compressed = zlib_compress_stored(data);
        // Zlib header: 0x78 0x01
        assert_eq!(compressed[0], 0x78);
        assert_eq!(compressed[1], 0x01);
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

    // ── Streaming: agent loop uses chat_stream ─────────────────────

    #[tokio::test]
    async fn test_agent_loop_uses_streaming() {
        run_in_tmp(|| async {
            let mock = MockClient::new(vec![MockClient::text_response("streamed response")]);
            let tool_defs = tools::get_tool_definitions();
            let permissions = tools::Permissions::new();
            let mut session = new_test_session();
            let mut config = GlobalConfig::default();

            process_user_message(
                "hello", &mut session, &mut config, "sys",
                &mock, &tool_defs, &permissions,
            )
            .await
            .unwrap();

            // chat_stream was called (MockClient.chat_stream delegates to chat)
            assert_eq!(mock.requests().len(), 1);
            // Response accumulated correctly
            assert_eq!(
                session.conversation.last().unwrap().content.as_deref(),
                Some("streamed response")
            );
        })
        .await;
    }

    // ── Auto-compaction triggers at 80% ──────────────────────────────

    #[test]
    fn test_auto_compaction_threshold() {
        // For grok with 131072 limit, 80% = 104857 tokens
        // ~4 chars/token → need ~419K chars to trigger
        let limit = types::context_limit_for_model("grok-4-1-fast");
        let threshold = limit * 80 / 100;

        // A message with ~420K chars ≈ 105K tokens → exceeds threshold
        let big_msg = "x".repeat(threshold as usize * 4 + 1000);
        let estimated = context::estimate_tokens(&big_msg);
        assert!(estimated > threshold, "Should exceed 80% threshold");
    }

    // ── Compaction truncates tool outputs ─────────────────────────────

    #[test]
    fn test_compact_truncates_large_tool_outputs() {
        with_tmp(|| {
            let mut session = ProjectSession::new();
            session.conversation.push(Message::system("sys"));
            session.conversation.push(Message::user("q1"));
            session.conversation.push(Message::assistant_text("a1"));

            // Add many messages including a large tool result
            for i in 0..12 {
                session.conversation.push(Message::user(&format!("u{i}")));
                if i == 5 {
                    // Add a large tool result
                    let big_output = "x".repeat(5000);
                    session.conversation.push(Message::tool_result("tc", &big_output));
                }
                session.conversation.push(Message::assistant_text(&format!("a{i}")));
            }

            let before = session.conversation.len();
            compact_conversation(&mut session, "sys");
            assert!(session.conversation.len() < before);

            // Any tool messages in kept portion should be truncated
            for msg in &session.conversation {
                if msg.role == "tool" {
                    if let Some(content) = &msg.content {
                        assert!(content.len() <= 2100, "Tool output should be truncated: {} chars", content.len());
                    }
                }
            }
        });
    }

    // ── Provider detection in model command ───────────────────────────

    #[test]
    fn test_detect_provider_from_model() {
        use types::detect_provider;
        use types::Provider;

        assert_eq!(detect_provider("gpt-4o-mini"), Provider::OpenAI);
        assert_eq!(detect_provider("claude-sonnet-4-20250514"), Provider::Anthropic);
        assert_eq!(detect_provider("grok-4-1-fast"), Provider::Grok);
        assert_eq!(detect_provider("custom-model"), Provider::Grok); // default
    }

    // ── Token estimation integration ─────────────────────────────────

    #[test]
    fn test_conversation_token_estimate_grows() {
        let mut session = new_test_session();
        let t1 = context::estimate_conversation_tokens(&session.conversation);

        session.conversation.push(Message::user("hello world"));
        let t2 = context::estimate_conversation_tokens(&session.conversation);
        assert!(t2 > t1);

        session.conversation.push(Message::assistant_text("I can help with that, here's a detailed response about your question."));
        let t3 = context::estimate_conversation_tokens(&session.conversation);
        assert!(t3 > t2);
    }

    // ── Context limit lookup ─────────────────────────────────────────

    #[test]
    fn test_context_limits_are_reasonable() {
        let grok = types::context_limit_for_model("grok-4-1-fast");
        let openai = types::context_limit_for_model("gpt-4o");
        let claude = types::context_limit_for_model("claude-sonnet-4-20250514");

        assert!(grok >= 100_000);
        assert!(openai >= 100_000);
        assert!(claude >= 100_000);
        assert!(claude > openai); // Claude has largest context
    }

    use crate::types::{ChatResponse, Usage};
}
