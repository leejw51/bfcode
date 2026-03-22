//! Telegram bot integration for bfcode.
//!
//! When TELEGRAM_BOT_TOKEN is set, `bfcode telegram` starts a long-polling bot
//! that forwards messages to the same LLM pipeline used by the interactive CLI.
//! Inspired by openclaw's grammy-based Telegram extension, adapted for Rust
//! with the teloxide framework.

use crate::{api, config, context, fallback, guard, mcp, persistence, plugin, tools, types};
use anyhow::{Context, Result};
use colored::Colorize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use types::{GlobalConfig, Message, ProjectSession, ToolDefinition};

/// Per-chat conversation state — each Telegram chat gets its own session.
struct ChatState {
    session: ProjectSession,
    config: GlobalConfig,
}

/// Shared bot state across all handlers.
struct BotState {
    chats: Mutex<HashMap<i64, ChatState>>,
    client: Box<dyn api::ChatClient>,
    tool_defs: Vec<ToolDefinition>,
    hook_mgr: plugin::HookManager,
    full_system_prompt: String,
    /// Optional: restrict to specific chat IDs. Empty = allow all.
    allowed_chat_ids: Vec<i64>,
}

/// Start the Telegram bot (long polling). Blocks until shutdown.
pub async fn run_telegram_bot() -> Result<()> {
    let token = std::env::var("TELEGRAM_BOT_TOKEN")
        .context("TELEGRAM_BOT_TOKEN environment variable not set")?;

    // Parse optional allowed chat IDs from TELEGRAM_ALLOWED_CHATS (comma-separated)
    let allowed_chat_ids: Vec<i64> = std::env::var("TELEGRAM_ALLOWED_CHATS")
        .unwrap_or_default()
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();

    // --- Reuse the same initialization as run_interactive ---
    let mut config = persistence::load_config();

    let instructions = persistence::load_instructions();
    let plans_context = persistence::load_plans_context();
    let context_files = context::load_context_files();
    let memories_context = persistence::load_memories_context();

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

    // Build LLM client
    let client: Box<dyn api::ChatClient> = if config.fallback_models.is_empty() {
        api::create_client(&config)?
    } else {
        Box::new(fallback::FallbackChain::build(
            &config.model,
            &config.fallback_models,
        )?)
    };

    let mut tool_defs = tools::get_tool_definitions();

    // Plugins
    let plugin_mgr = plugin::PluginManager::load();
    let plugin_tools = plugin_mgr.get_tool_definitions();
    if !plugin_tools.is_empty() {
        tool_defs.extend(plugin_tools);
    }
    let mut hook_mgr = plugin::HookManager::load();
    for hook_config in plugin_mgr.get_hook_configs() {
        hook_mgr.add_hook(hook_config);
    }
    plugin::set_plugin_manager(plugin_mgr).await;

    // MCP servers
    let full_config = config::load_full_config().unwrap_or_else(|_| config::FullConfig {
        model: config.model.clone(),
        temperature: config.temperature,
        provider: format!("{}", types::detect_provider(&config.model)),
        gateway: None,
        daemon: None,
        hooks: Vec::new(),
        env: std::collections::HashMap::new(),
        include: Vec::new(),
        fallback_models: Vec::new(),
        mcp_servers: std::collections::HashMap::new(),
        config_version: 2,
    });
    if !full_config.mcp_servers.is_empty() {
        eprintln!("{}", "MCP servers:".dimmed());
        let mcp_manager = mcp::McpManager::connect_all(&full_config.mcp_servers).await;
        let mcp_tools = mcp_manager.get_tool_definitions();
        tool_defs.extend(mcp_tools);
        tools::set_mcp_manager(mcp_manager).await;
    }

    let state = Arc::new(BotState {
        chats: Mutex::new(HashMap::new()),
        client,
        tool_defs,
        hook_mgr,
        full_system_prompt,
        allowed_chat_ids,
    });

    // --- Telegram long polling via reqwest ---
    let http = reqwest::Client::new();
    let api_base = format!("https://api.telegram.org/bot{token}");

    // Get bot info
    let me: serde_json::Value = http
        .get(format!("{api_base}/getMe"))
        .send()
        .await?
        .json()
        .await?;
    let bot_name = me["result"]["first_name"].as_str().unwrap_or("bfcode bot");
    let bot_username = me["result"]["username"].as_str().unwrap_or("bfcode_bot");

    println!("{}", "bfcode Telegram bot".green().bold());
    println!("Bot:      {} (@{})", bot_name.cyan(), bot_username);
    println!("Model:    {}", config.model.cyan());
    if !state.allowed_chat_ids.is_empty() {
        println!(
            "Allowed:  {} chat(s)",
            state.allowed_chat_ids.len().to_string().cyan()
        );
    } else {
        println!("Allowed:  {}", "all chats".yellow());
    }
    println!();
    println!("Listening for messages...");

    let mut offset: i64 = 0;

    loop {
        // Long-poll for updates (30s timeout)
        let updates: serde_json::Value = match http
            .get(format!("{api_base}/getUpdates"))
            .query(&[
                ("offset", offset.to_string()),
                ("timeout", "30".into()),
                ("allowed_updates", r#"["message"]"#.into()),
            ])
            .send()
            .await
        {
            Ok(resp) => match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("  {} Parse error: {e}", "✗".red());
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    continue;
                }
            },
            Err(e) => {
                eprintln!("  {} Poll error: {e}", "✗".red());
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        let results = match updates["result"].as_array() {
            Some(arr) => arr,
            None => continue,
        };

        for update in results {
            // Advance offset
            if let Some(uid) = update["update_id"].as_i64() {
                offset = uid + 1;
            }

            let msg = &update["message"];
            let chat_id = match msg["chat"]["id"].as_i64() {
                Some(id) => id,
                None => continue,
            };
            let text = match msg["text"].as_str() {
                Some(t) if !t.is_empty() => t.to_string(),
                _ => continue, // skip non-text for now
            };
            let from = msg["from"]["first_name"].as_str().unwrap_or("Unknown");

            // Access control
            if !state.allowed_chat_ids.is_empty() && !state.allowed_chat_ids.contains(&chat_id) {
                eprintln!(
                    "  {} Blocked message from chat {chat_id} ({})",
                    "⊘".yellow(),
                    from
                );
                continue;
            }

            eprintln!(
                "  {} [chat:{chat_id}] {}: {}",
                "←".cyan(),
                from,
                truncate_display(&text, 80)
            );

            // Handle /start and /help commands
            if text == "/start" || text == "/help" {
                let help_text = format!(
                    "🤖 *bfcode* — AI coding assistant\n\n\
                     Send me any message and I'll respond using *{}*.\n\n\
                     Commands:\n\
                     /model — Show current model\n\
                     /clear — Reset conversation\n\
                     /help — Show this message",
                    config.model
                );
                send_message(&http, &api_base, chat_id, &help_text).await;
                continue;
            }

            if text == "/model" {
                send_message(
                    &http,
                    &api_base,
                    chat_id,
                    &format!("Current model: `{}`", config.model),
                )
                .await;
                continue;
            }

            if text == "/clear" {
                let mut chats = state.chats.lock().await;
                chats.remove(&chat_id);
                send_message(&http, &api_base, chat_id, "Conversation cleared.").await;
                eprintln!("  {} [chat:{chat_id}] Session cleared", "↻".yellow());
                continue;
            }

            // Send "typing" indicator
            let _ = http
                .post(format!("{api_base}/sendChatAction"))
                .json(&serde_json::json!({
                    "chat_id": chat_id,
                    "action": "typing",
                }))
                .send()
                .await;

            // Process message through the LLM pipeline
            let state_clone = state.clone();
            let http_clone = http.clone();
            let api_base_clone = api_base.clone();

            // Process inline to keep ordering per-chat simple
            let response_text = process_telegram_message(&state_clone, chat_id, &text).await;

            // Send response (split into chunks if needed — Telegram max is 4096 chars)
            let chunks = split_message(&response_text, 4000);
            for chunk in &chunks {
                send_message(&http_clone, &api_base_clone, chat_id, chunk).await;
            }

            eprintln!(
                "  {} [chat:{chat_id}] Sent {} chunk(s), {} chars",
                "→".green(),
                chunks.len(),
                response_text.len()
            );
        }
    }
}

/// Process a user message through the LLM agent loop, returning the assistant's
/// text response. This mirrors `process_user_message` from main.rs but collects
/// output as a string instead of printing to stdout.
async fn process_telegram_message(state: &BotState, chat_id: i64, input: &str) -> String {
    let mut chats = state.chats.lock().await;

    // Get or create per-chat state
    let chat_state = chats.entry(chat_id).or_insert_with(|| {
        let mut session = ProjectSession::new();
        session.title = format!("telegram-{chat_id}");
        session
            .conversation
            .push(Message::system(&state.full_system_prompt));
        ChatState {
            session,
            config: persistence::load_config(),
        }
    });

    let session = &mut chat_state.session;
    let config = &chat_state.config;

    // Add user message
    session.conversation.push(Message::user(input));

    // Context window guard
    let ctx_check = guard::check_context_window(&session.conversation, &config.model);
    match ctx_check.status {
        guard::ContextStatus::Blocked
        | guard::ContextStatus::PreemptiveOverflow
        | guard::ContextStatus::Warning => {
            // Simple compaction: keep system + last 10 messages
            if session.conversation.len() > 12 {
                let system_msg = session.conversation[0].clone();
                let tail: Vec<Message> = session
                    .conversation
                    .iter()
                    .rev()
                    .take(10)
                    .cloned()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                session.conversation = vec![system_msg];
                session.conversation.extend(tail);
                eprintln!("  {} [chat:{chat_id}] Compacted conversation", "~".yellow());
            }
        }
        guard::ContextStatus::Ok => {}
    }

    // Auto-approve tools in Telegram mode (no interactive terminal)
    let permissions = tools::Permissions::new_auto_approve();
    let session_id = session.id.clone();

    // Agent loop (max 25 rounds)
    let mut final_text = String::new();
    for _round in 0..25 {
        let messages = session.conversation.clone();
        let model = config.model.clone();
        let temp = config.temperature;

        // Use non-streaming chat for Telegram (collect full response)
        let response = match state
            .client
            .chat(&messages, &state.tool_defs, &model, temp)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let err_msg = format!("Error: {e}");
                eprintln!("  {} [chat:{chat_id}] {err_msg}", "✗".red());
                return err_msg;
            }
        };

        // Track tokens
        if let Some(usage) = &response.usage {
            session.total_tokens += usage.total_tokens;
            let cost =
                types::calculate_cost(&config.model, usage.prompt_tokens, usage.completion_tokens);
            eprintln!(
                "  {} [chat:{chat_id}] tokens: {} | cost: {}",
                "~".dimmed(),
                usage.total_tokens,
                types::format_cost(cost)
            );
        }

        if response.choices.is_empty() {
            return "Error: empty response from API".into();
        }

        let assistant_msg = &response.choices[0].message;

        // Handle tool calls
        if let Some(tool_calls) = &assistant_msg.tool_calls {
            session
                .conversation
                .push(Message::assistant_tool_calls(tool_calls.clone()));

            for tc in tool_calls {
                eprintln!(
                    "  {} [chat:{chat_id}] tool: {}",
                    ">>>".cyan(),
                    tc.function.name
                );

                let result = tools::execute_tool(
                    &tc.function.name,
                    &tc.function.arguments,
                    &permissions,
                    &session_id,
                )
                .await;
                let result = guard::truncate_tool_result(&result, &config.model);
                session
                    .conversation
                    .push(Message::tool_result(&tc.id, &result));
            }
            continue; // next round — let LLM process tool results
        }

        // Text response — done
        if let Some(content) = &assistant_msg.content {
            session.conversation.push(Message::assistant_text(content));
            final_text = content.clone();
        }
        break;
    }

    if final_text.is_empty() {
        final_text = "(no response)".into();
    }

    final_text
}

/// Send a text message via Telegram Bot API.
async fn send_message(http: &reqwest::Client, api_base: &str, chat_id: i64, text: &str) {
    let payload = serde_json::json!({
        "chat_id": chat_id,
        "text": text,
        "parse_mode": "Markdown",
    });
    match http
        .post(format!("{api_base}/sendMessage"))
        .json(&payload)
        .send()
        .await
    {
        Ok(resp) => {
            if !resp.status().is_success() {
                // Retry without Markdown parse_mode (in case of formatting issues)
                let plain = serde_json::json!({
                    "chat_id": chat_id,
                    "text": text,
                });
                let _ = http
                    .post(format!("{api_base}/sendMessage"))
                    .json(&plain)
                    .send()
                    .await;
            }
        }
        Err(e) => {
            eprintln!("  {} Failed to send message: {e}", "✗".red());
        }
    }
}

/// Split a message into chunks at a maximum character length, trying to break
/// at newline boundaries.
fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= max_len {
            chunks.push(remaining.to_string());
            break;
        }

        // Try to break at last newline within limit
        let boundary = &remaining[..max_len];
        let split_at = boundary.rfind('\n').unwrap_or_else(|| {
            // Fall back to last space
            boundary.rfind(' ').unwrap_or(max_len)
        });

        let split_at = if split_at == 0 { max_len } else { split_at };

        chunks.push(remaining[..split_at].to_string());
        remaining = remaining[split_at..].trim_start_matches('\n');
    }

    chunks
}

/// Truncate text for display in log output.
fn truncate_display(text: &str, max_len: usize) -> String {
    let single_line = text.replace('\n', " ");
    if single_line.len() <= max_len {
        single_line
    } else {
        format!("{}...", &single_line[..max_len])
    }
}

/// Check if TELEGRAM_BOT_TOKEN is set in the environment.
pub fn is_telegram_enabled() -> bool {
    std::env::var("TELEGRAM_BOT_TOKEN").is_ok()
}
