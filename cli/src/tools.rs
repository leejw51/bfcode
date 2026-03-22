use crate::mcp::McpManager;
use crate::types::*;
use anyhow::{Context, Result, bail, ensure};
use colored::Colorize;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Session-scoped todo list (thread-safe, keyed by session_id)
static SESSION_TODOS: std::sync::LazyLock<Mutex<std::collections::HashMap<String, Vec<TodoItem>>>> =
    std::sync::LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// Global MCP manager (set once during startup)
static MCP_MANAGER: std::sync::LazyLock<tokio::sync::Mutex<Option<McpManager>>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(None));

/// Initialize the global MCP manager.
pub async fn set_mcp_manager(manager: McpManager) {
    let mut guard = MCP_MANAGER.lock().await;
    *guard = Some(manager);
}

/// Get MCP tool definitions from the global manager.
pub async fn get_mcp_tool_definitions() -> Vec<ToolDefinition> {
    let guard = MCP_MANAGER.lock().await;
    match &*guard {
        Some(manager) => manager.get_tool_definitions(),
        None => Vec::new(),
    }
}

/// Shutdown all MCP servers.
pub async fn shutdown_mcp() {
    let guard = MCP_MANAGER.lock().await;
    if let Some(manager) = &*guard {
        manager.shutdown_all().await;
    }
}

/// Execute a plugin tool via the global plugin manager.
pub async fn execute_plugin_tool(name: &str, arguments: &str) -> Result<String> {
    crate::plugin::execute_plugin_tool(name, arguments).await
}

/// Agent mode — determines which tools are available
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AgentMode {
    /// Full access to all tools (default)
    Build,
    /// Read-only mode — write/edit/bash disabled except .bfcode/plans/
    Plan,
    /// Exploration mode — only read/search tools allowed
    Explore,
}

impl std::fmt::Display for AgentMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentMode::Build => write!(f, "build"),
            AgentMode::Plan => write!(f, "plan"),
            AgentMode::Explore => write!(f, "explore"),
        }
    }
}

/// Current agent mode (process-global)
static AGENT_MODE: std::sync::LazyLock<Mutex<AgentMode>> =
    std::sync::LazyLock::new(|| Mutex::new(AgentMode::Build));

// Output limits (inspired by opencode)
const MAX_OUTPUT_LINES: usize = 2000;
const MAX_OUTPUT_BYTES: usize = 51200; // 50 KB
const DEFAULT_READ_LIMIT: u64 = 2000;
const MAX_CHARS_PER_LINE: usize = 2000;

pub fn get_tool_definitions() -> Vec<ToolDefinition> {
    let has_search_key =
        std::env::var("BRAVE_API_KEY").is_ok() || std::env::var("TAVILY_API_KEY").is_ok();
    let has_openai_key = std::env::var("OPENAI_API_KEY").is_ok();

    let mut tools = vec![
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "read".into(),
                description: "Read file contents with line numbers. Use offset/limit for large files.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Absolute or relative file path to read"},
                        "offset": {"type": "integer", "description": "Line number to start reading from (1-indexed). Optional."},
                        "limit": {"type": "integer", "description": "Maximum number of lines to read. Default 2000."}
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "write".into(),
                description: "Create or overwrite a file with new content. Creates parent directories if needed.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "File path to write"},
                        "content": {"type": "string", "description": "Complete file content to write"}
                    },
                    "required": ["path", "content"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "edit".into(),
                description: "Edit a file by replacing an exact string match with new content. The old_string must match exactly (including whitespace/indentation). Prefer this over write for modifying existing files.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "File path to edit"},
                        "old_string": {"type": "string", "description": "Exact string to find and replace. Must be unique in the file."},
                        "new_string": {"type": "string", "description": "Replacement string. Must differ from old_string."},
                        "replace_all": {"type": "boolean", "description": "Replace all occurrences. Default false."}
                    },
                    "required": ["path", "old_string", "new_string"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "bash".into(),
                description: "Run a shell command. Returns exit code, stdout, and stderr. Default timeout 120s.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {"type": "string", "description": "Shell command to execute"},
                        "timeout": {"type": "integer", "description": "Timeout in seconds. Default 120."}
                    },
                    "required": ["command"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "glob".into(),
                description: "Find files matching a glob pattern. Returns sorted file paths.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "pattern": {"type": "string", "description": "Glob pattern (e.g. \"**/*.rs\", \"src/**/*.ts\")"},
                        "path": {"type": "string", "description": "Base directory to search in. Defaults to current directory."}
                    },
                    "required": ["pattern"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "grep".into(),
                description: "Search file contents using a regex pattern. Returns matching lines with file paths and line numbers.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "pattern": {"type": "string", "description": "Regex pattern to search for"},
                        "path": {"type": "string", "description": "Directory to search in. Defaults to current directory."},
                        "include": {"type": "string", "description": "File glob filter (e.g. \"*.rs\", \"*.ts\"). Optional."}
                    },
                    "required": ["pattern"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "apply_patch".into(),
                description: "Apply a unified diff patch to one or more files. Use standard format with --- a/file, +++ b/file, @@ hunk headers.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "patch": {"type": "string", "description": "Unified diff content to apply"}
                    },
                    "required": ["patch"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "list_files".into(),
                description: "List files and directories at the given path with type indicators.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Directory path to list"}
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "webfetch".into(),
                description: "Fetch content from a URL. HTML is automatically stripped to plain text. Use this to read documentation, web pages, or API responses.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "url": {"type": "string", "description": "URL to fetch"}
                    },
                    "required": ["url"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "memory_save".into(),
                description: "Save a context memory as a markdown file. Saved to .bfcode/memory/ by default, or to a specific folder. Use this to remember important context across sessions.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "Memory name (used as filename slug)"},
                        "description": {"type": "string", "description": "One-line summary of what this memory is about"},
                        "memory_type": {"type": "string", "enum": ["user", "feedback", "project", "reference"], "description": "Memory type"},
                        "content": {"type": "string", "description": "Markdown content of the memory"},
                        "folder": {"type": "string", "description": "Optional folder to save in (default: .bfcode/memory/)"}
                    },
                    "required": ["name", "description", "memory_type", "content"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "memory_delete".into(),
                description: "Delete a context memory by name from .bfcode/memory/.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "Memory name to delete"}
                    },
                    "required": ["name"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "memory_list".into(),
                description: "List all saved context memories in .bfcode/memory/.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {}
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "memory_search".into(),
                description: "Search saved memories semantically using TF-IDF. Returns the most relevant memories matching the query.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Search query"},
                        "top_k": {"type": "integer", "description": "Number of results to return. Default 5."}
                    },
                    "required": ["query"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "pdf_read".into(),
                description: "Extract text content from a PDF file. Returns text with page markers.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Path to the PDF file"},
                        "pages": {"type": "string", "description": "Page range to read (e.g. \"1-5\", \"3\", \"10-20\"). Optional, defaults to all pages."}
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "tts".into(),
                description: "Convert text to speech audio. Uses system TTS (say/espeak) or OpenAI TTS API.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "text": {"type": "string", "description": "Text to convert to speech"},
                        "voice": {"type": "string", "description": "Voice name. For system: macOS voice names. For API: alloy, echo, fable, onyx, nova, shimmer."},
                        "output_path": {"type": "string", "description": "Path to save audio file. If provided, saves to file instead of playing."}
                    },
                    "required": ["text"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "browser_navigate".into(),
                description: "Navigate a headless browser to a URL and return page content as text. Requires Chrome/Chromium installed.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "url": {"type": "string", "description": "URL to navigate to"}
                    },
                    "required": ["url"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "browser_screenshot".into(),
                description: "Take a screenshot of the current browser page.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "output_path": {"type": "string", "description": "Path to save the screenshot PNG. Default: auto-generated."}
                    }
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "browser_click".into(),
                description: "Click an element by CSS selector in the headless browser.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "selector": {"type": "string", "description": "CSS selector of the element to click"}
                    },
                    "required": ["selector"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "browser_type".into(),
                description: "Type text into a form element by CSS selector in the headless browser.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "selector": {"type": "string", "description": "CSS selector of the input element"},
                        "text": {"type": "string", "description": "Text to type into the element"}
                    },
                    "required": ["selector", "text"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "browser_evaluate".into(),
                description: "Evaluate JavaScript in the headless browser and return the result.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "script": {"type": "string", "description": "JavaScript code to evaluate"}
                    },
                    "required": ["script"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "browser_close".into(),
                description: "Close the headless browser and free resources.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {}
                }),
            },
        },
        // --- Multi-Edit Tool ---
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "multiedit".into(),
                description: "Apply multiple find-and-replace edits to a single file in one atomic operation. Edits are applied sequentially so each edit sees the result of the previous one. Prevents repeated tool round-trips.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Absolute or relative file path to edit"},
                        "edits": {
                            "type": "array",
                            "description": "Array of edit operations to apply sequentially",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "old_string": {"type": "string", "description": "Exact string to find and replace"},
                                    "new_string": {"type": "string", "description": "Replacement string"},
                                    "replace_all": {"type": "boolean", "description": "Replace all occurrences. Default false."}
                                },
                                "required": ["old_string", "new_string"]
                            }
                        }
                    },
                    "required": ["path", "edits"]
                }),
            },
        },
        // --- Batch Tool ---
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "batch".into(),
                description: "Execute multiple independent tool calls in parallel (up to 25). Returns results for each call. Cannot nest batch calls. Great for parallel reads, searches, or independent edits across different files.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "tool_calls": {
                            "type": "array",
                            "description": "Array of tool calls to execute in parallel (max 25)",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "tool": {"type": "string", "description": "Tool name to call"},
                                    "parameters": {"type": "object", "description": "Tool parameters as a JSON object"}
                                },
                                "required": ["tool", "parameters"]
                            }
                        }
                    },
                    "required": ["tool_calls"]
                }),
            },
        },
        // --- Task/Subagent Tool ---
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "task".into(),
                description: "Launch a subagent to handle a complex multi-step task autonomously. The subagent runs in a separate session with its own context. Use for exploration, planning, or independent work that shouldn't pollute the main conversation.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "description": {"type": "string", "description": "Short 3-5 word task description"},
                        "prompt": {"type": "string", "description": "Full task prompt for the subagent"},
                        "subagent_type": {"type": "string", "enum": ["explore", "plan", "build"], "description": "Type of subagent: explore (read-only research), plan (create a plan), build (implement changes). Default: explore."}
                    },
                    "required": ["description", "prompt"]
                }),
            },
        },
        // --- Todo Tools ---
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "todowrite".into(),
                description: "Write/update the session todo list for tracking progress. Replaces the entire todo list. Use to track multi-step work within a session.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "todos": {
                            "type": "array",
                            "description": "Complete todo list (replaces existing)",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "content": {"type": "string", "description": "Brief description of the task"},
                                    "status": {"type": "string", "enum": ["pending", "in_progress", "completed", "cancelled"], "description": "Task status"},
                                    "priority": {"type": "string", "enum": ["high", "medium", "low"], "description": "Task priority. Default: medium."}
                                },
                                "required": ["content", "status"]
                            }
                        }
                    },
                    "required": ["todos"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "todoread".into(),
                description: "Read the current session todo list. Returns all todos with their status and priority.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {}
                }),
            },
        },
        // --- Plan Mode Tools ---
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "plan_enter".into(),
                description: "Enter plan mode (read-only phase). In plan mode, write/edit/apply_patch tools are disabled — only reads, searches, and writing to .bfcode/plans/ are allowed. Use this to design a detailed plan before implementation.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "plan_name": {"type": "string", "description": "Optional name for the plan being created"}
                    }
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "plan_exit".into(),
                description: "Exit plan mode and return to build mode where all tools are available. Call this when planning is complete and you're ready to implement.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {}
                }),
            },
        },
        // --- LSP Tool ---
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "lsp".into(),
                description: "Code intelligence via Language Server Protocol. Supports Rust (rust-analyzer), Go (gopls), and TypeScript/JavaScript (typescript-language-server). Operations: goToDefinition, findReferences, hover, documentSymbol, workspaceSymbol. Line and character are 1-based.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "operation": {
                            "type": "string",
                            "enum": ["goToDefinition", "findReferences", "hover", "documentSymbol", "workspaceSymbol"],
                            "description": "LSP operation to perform"
                        },
                        "filePath": {"type": "string", "description": "File path (absolute or relative)"},
                        "line": {"type": "integer", "description": "Line number (1-based). Required for goToDefinition, findReferences, hover."},
                        "character": {"type": "integer", "description": "Column number (1-based). Required for goToDefinition, findReferences, hover."},
                        "query": {"type": "string", "description": "Search query for workspaceSymbol. Optional."}
                    },
                    "required": ["operation", "filePath"]
                }),
            },
        },
    ];

    // Conditionally add tools that require API keys
    if has_search_key {
        tools.push(ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "websearch".into(),
                description: "Search the web using Brave Search or Tavily API. Returns titles, URLs, and snippets.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Search query"},
                        "num_results": {"type": "integer", "description": "Number of results to return. Default 5."}
                    },
                    "required": ["query"]
                }),
            },
        });
    }

    if has_openai_key {
        tools.push(ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "image_generate".into(),
                description: "Generate an image using DALL-E API. Saves the image locally and returns the file path.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "prompt": {"type": "string", "description": "Image generation prompt describing what to create"},
                        "size": {"type": "string", "description": "Image size: \"1024x1024\", \"1792x1024\", \"1024x1792\". Default \"1024x1024\"."},
                        "output_path": {"type": "string", "description": "Path to save the image. Default: .bfcode/generated/<timestamp>.png"}
                    },
                    "required": ["prompt"]
                }),
            },
        });
    }

    tools
}

/// Permission tracker for tool execution (like opencode's permission system)
pub struct Permissions {
    /// Tool patterns that are always allowed (e.g., "bash:cargo *", "write:*")
    always_allowed: Mutex<HashSet<String>>,
    /// When true, auto-approve all tool calls without prompting
    pub auto_approve: AtomicBool,
}

impl Permissions {
    pub fn new() -> Self {
        Self {
            always_allowed: Mutex::new(HashSet::new()),
            auto_approve: AtomicBool::new(false),
        }
    }

    pub fn new_auto_approve() -> Self {
        Self {
            always_allowed: Mutex::new(HashSet::new()),
            auto_approve: AtomicBool::new(true),
        }
    }

    /// Check if a tool action is always allowed
    fn is_allowed(&self, key: &str) -> bool {
        let Ok(allowed) = self.always_allowed.lock() else {
            return false;
        };
        let prefix = key.split(':').next().unwrap_or("");
        allowed.contains(key) || allowed.contains(&format!("{prefix}:*"))
    }

    /// Add a pattern to always-allowed list
    fn allow_always(&self, key: &str) {
        if let Ok(mut allowed) = self.always_allowed.lock() {
            allowed.insert(key.to_string());
        }
    }

    /// Ask user for permission. Returns true if allowed.
    fn ask_permission(&self, tool: &str, summary: &str) -> PermissionReply {
        let key = format!("{tool}:{summary}");
        if self.is_allowed(&key) {
            return PermissionReply::Allow;
        }

        // In oneshot mode (gateway subprocess), auto-approve all tools
        // since there is no TTY to prompt the user.
        if self.auto_approve.load(Ordering::Relaxed) {
            eprintln!(
                "  {} {} {} {}",
                "✓".green(),
                format!("Auto-allow {tool}:").white().bold(),
                summary,
                "(auto-approved)".dimmed()
            );
            return PermissionReply::Allow;
        }

        eprint!(
            "  {} {} {} ",
            "?".yellow().bold(),
            format!("Allow {tool}:").white().bold(),
            summary
        );
        eprint!("{}", " [y/n/a] ".dimmed());
        let _ = std::io::Write::flush(&mut std::io::stderr());

        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).unwrap_or(0) == 0 {
            return PermissionReply::Deny;
        }

        match input.trim().to_lowercase().as_str() {
            "y" | "yes" => PermissionReply::Allow,
            "a" | "always" => {
                // Auto-approve all tools for this session
                self.auto_approve.store(true, Ordering::Relaxed);
                eprintln!(
                    "  {} {}",
                    "✓".green(),
                    "All tools auto-approved for this session".dimmed()
                );
                PermissionReply::Allow
            }
            _ => PermissionReply::Deny,
        }
    }
}

enum PermissionReply {
    Allow,
    Deny,
}

pub fn execute_tool<'a>(
    name: &'a str,
    arguments: &'a str,
    permissions: &'a Permissions,
    session_id: &'a str,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + 'a>> {
    Box::pin(execute_tool_inner(name, arguments, permissions, session_id))
}

async fn execute_tool_inner(
    name: &str,
    arguments: &str,
    permissions: &Permissions,
    session_id: &str,
) -> String {
    // Agent mode restrictions
    let mode = current_agent_mode();
    match mode {
        AgentMode::Explore => {
            // Explore mode: only read/search tools allowed
            let allowed = matches!(
                name,
                "read"
                    | "glob"
                    | "grep"
                    | "list_files"
                    | "lsp"
                    | "webfetch"
                    | "websearch"
                    | "memory_list"
                    | "memory_search"
                    | "pdf_read"
                    | "todoread"
                    | "todowrite"
                    | "plan_enter"
                    | "plan_exit"
            );
            if !allowed {
                return format!(
                    "Error: Tool '{name}' is disabled in explore mode. \
                     Only read/search tools are available. \
                     Use plan_exit to switch to build mode."
                );
            }
        }
        AgentMode::Plan => {
            // Plan mode: block write tools except .bfcode/plans/
            if matches!(
                name,
                "write" | "edit" | "apply_patch" | "multiedit" | "bash"
            ) {
                let is_plan_write = if matches!(name, "write" | "edit" | "multiedit") {
                    serde_json::from_str::<serde_json::Value>(arguments)
                        .ok()
                        .and_then(|v| {
                            v.get("path")?
                                .as_str()
                                .map(|s| s.contains(".bfcode/plans/"))
                        })
                        .unwrap_or(false)
                } else {
                    false
                };
                if !is_plan_write {
                    return format!(
                        "Error: Tool '{name}' is disabled in plan mode. \
                         Use plan_exit to switch to build mode before making changes. \
                         In plan mode, only read/search tools and writing to .bfcode/plans/ are allowed."
                    );
                }
            }
        }
        AgentMode::Build => {} // all tools available
    }

    // Check protected files for write/edit/apply_patch/multiedit
    if matches!(name, "write" | "edit" | "apply_patch" | "multiedit") {
        if let Some(msg) = check_protected_file(name, arguments) {
            return msg;
        }
    }

    // Check permissions for dangerous tools
    let needs_permission = matches!(
        name,
        "bash"
            | "write"
            | "edit"
            | "apply_patch"
            | "multiedit"
            | "memory_save"
            | "memory_delete"
            | "browser_navigate"
            | "browser_click"
            | "browser_type"
            | "browser_evaluate"
            | "image_generate"
            | "tts"
    ) || name.starts_with("mcp_")
        || name.starts_with("plugin_");
    if needs_permission {
        let summary = tool_permission_summary(name, arguments);
        match permissions.ask_permission(name, &summary) {
            PermissionReply::Allow => {}
            PermissionReply::Deny => {
                return format!(
                    "Error: User denied permission for {name}. Try a different approach or ask the user."
                );
            }
        }
    }

    let result = match name {
        "read" => exec_read(arguments).await,
        "write" => exec_write(arguments, session_id).await,
        "edit" => exec_edit(arguments, session_id).await,
        "bash" => exec_bash(arguments).await,
        "glob" => exec_glob(arguments).await,
        "grep" => exec_grep(arguments).await,
        "list_files" => exec_list_files(arguments).await,
        "apply_patch" => exec_apply_patch(arguments, session_id).await,
        "webfetch" => exec_webfetch(arguments).await,
        "websearch" => exec_websearch(arguments).await,
        "memory_save" => exec_memory_save(arguments).await,
        "memory_delete" => exec_memory_delete(arguments).await,
        "memory_list" => exec_memory_list().await,
        "memory_search" => exec_memory_search(arguments).await,
        "pdf_read" => exec_pdf_read(arguments).await,
        "image_generate" => exec_image_generate(arguments).await,
        "tts" => exec_tts(arguments).await,
        "browser_navigate" => exec_browser_navigate(arguments).await,
        "browser_screenshot" => exec_browser_screenshot(arguments).await,
        "browser_click" => exec_browser_click(arguments).await,
        "browser_type" => exec_browser_type(arguments).await,
        "browser_evaluate" => exec_browser_evaluate(arguments).await,
        "browser_close" => exec_browser_close().await,
        "multiedit" => exec_multiedit(arguments, session_id).await,
        "batch" => exec_batch(arguments, permissions, session_id).await,
        "task" => exec_task(arguments, permissions, session_id).await,
        "todowrite" => exec_todowrite(arguments, session_id).await,
        "todoread" => exec_todoread(session_id).await,
        "plan_enter" => exec_plan_enter(arguments).await,
        "plan_exit" => exec_plan_exit().await,
        "lsp" => crate::lsp::execute(arguments).await,
        _ if name.starts_with("mcp_") => {
            // Dispatch to MCP manager
            let guard = MCP_MANAGER.lock().await;
            match &*guard {
                Some(manager) => manager.execute_tool(name, arguments).await,
                None => Err(anyhow::anyhow!("MCP not initialized")),
            }
        }
        _ if name.starts_with("plugin_") => {
            // Dispatch to plugin manager
            execute_plugin_tool(name, arguments).await
        }
        _ => Err(anyhow::anyhow!("Unknown tool: {name}")),
    };

    match result {
        Ok(output) => truncate_output(&output),
        Err(e) => format!("Error: {e}"),
    }
}

/// Generate a short summary for the permission prompt
fn tool_permission_summary(name: &str, arguments: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(arguments) {
        Ok(v) => match name {
            "bash" => v["command"].as_str().unwrap_or("").to_string(),
            "write" => {
                let path = v["path"].as_str().unwrap_or("");
                let bytes = v["content"].as_str().map(|s| s.len()).unwrap_or(0);
                format!("{path} ({bytes} bytes)")
            }
            "edit" => {
                let path = v["path"].as_str().unwrap_or("");
                format!("{path}")
            }
            "apply_patch" => {
                let patch = v["patch"].as_str().unwrap_or("");
                let file_count = patch.matches("+++ ").count();
                format!("{file_count} file(s)")
            }
            "multiedit" => {
                let path = v["path"].as_str().unwrap_or("");
                let count = v["edits"].as_array().map(|a| a.len()).unwrap_or(0);
                format!("{path} ({count} edits)")
            }
            _ => arguments.to_string(),
        },
        Err(_) => arguments.to_string(),
    }
}

/// Print tool call info before execution
pub fn print_tool_call(name: &str, arguments: &str) {
    let summary = match serde_json::from_str::<serde_json::Value>(arguments) {
        Ok(v) => match name {
            "read" => {
                let path = v["path"].as_str().unwrap_or("");
                let offset = v["offset"].as_u64();
                let limit = v["limit"].as_u64();
                match (offset, limit) {
                    (Some(o), Some(l)) => format!("{path} (lines {o}-{})", o + l),
                    (Some(o), None) => format!("{path} (from line {o})"),
                    _ => path.to_string(),
                }
            }
            "write" => format!(
                "{} ({} bytes)",
                v["path"].as_str().unwrap_or(""),
                v["content"].as_str().map(|s| s.len()).unwrap_or(0)
            ),
            "edit" => {
                let path = v["path"].as_str().unwrap_or("");
                let old_len = v["old_string"].as_str().map(|s| s.len()).unwrap_or(0);
                let new_len = v["new_string"].as_str().map(|s| s.len()).unwrap_or(0);
                format!("{path} ({old_len} -> {new_len} chars)")
            }
            "bash" => v["command"].as_str().unwrap_or("").to_string(),
            "glob" => {
                let pattern = v["pattern"].as_str().unwrap_or("");
                let path = v["path"].as_str().unwrap_or(".");
                format!("{pattern} in {path}")
            }
            "grep" => {
                let pattern = v["pattern"].as_str().unwrap_or("");
                let path = v["path"].as_str().unwrap_or(".");
                format!("\"{pattern}\" in {path}")
            }
            "list_files" => v["path"].as_str().unwrap_or("").to_string(),
            "memory_save" => {
                let name = v["name"].as_str().unwrap_or("");
                let folder = v["folder"].as_str().unwrap_or(".bfcode/memory/");
                format!("{name} -> {folder}")
            }
            "memory_delete" => v["name"].as_str().unwrap_or("").to_string(),
            "memory_search" => {
                let query = v["query"].as_str().unwrap_or("");
                format!("\"{query}\"")
            }
            "websearch" => {
                let query = v["query"].as_str().unwrap_or("");
                format!("\"{query}\"")
            }
            "pdf_read" => {
                let path = v["path"].as_str().unwrap_or("");
                let pages = v["pages"].as_str().unwrap_or("all");
                format!("{path} (pages: {pages})")
            }
            "image_generate" => {
                let prompt = v["prompt"].as_str().unwrap_or("");
                let short: String = prompt.chars().take(60).collect();
                format!("\"{short}...\"")
            }
            "tts" => {
                let text = v["text"].as_str().unwrap_or("");
                let short: String = text.chars().take(50).collect();
                format!("\"{short}...\"")
            }
            "browser_navigate" => v["url"].as_str().unwrap_or("").to_string(),
            "browser_screenshot" => v["output_path"].as_str().unwrap_or("auto").to_string(),
            "browser_click" => v["selector"].as_str().unwrap_or("").to_string(),
            "browser_type" => {
                let sel = v["selector"].as_str().unwrap_or("");
                format!("{sel}")
            }
            "browser_evaluate" => {
                let script = v["script"].as_str().unwrap_or("");
                let short: String = script.chars().take(60).collect();
                format!("{short}...")
            }
            "browser_close" => "closing browser".to_string(),
            "multiedit" => {
                let path = v["path"].as_str().unwrap_or("");
                let count = v["edits"].as_array().map(|a| a.len()).unwrap_or(0);
                format!("{path} ({count} edits)")
            }
            "batch" => {
                let count = v["tool_calls"].as_array().map(|a| a.len()).unwrap_or(0);
                format!("{count} tool calls in parallel")
            }
            "task" => {
                let desc = v["description"].as_str().unwrap_or("");
                let agent = v["subagent_type"].as_str().unwrap_or("explore");
                format!("{desc} ({agent})")
            }
            "todowrite" => {
                let count = v["todos"].as_array().map(|a| a.len()).unwrap_or(0);
                format!("{count} todos")
            }
            "todoread" => "reading todos".to_string(),
            "plan_enter" => {
                let name = v["plan_name"].as_str().unwrap_or("unnamed");
                format!("entering plan mode ({name})")
            }
            "plan_exit" => "exiting plan mode".to_string(),
            _ => arguments.to_string(),
        },
        Err(_) => arguments.to_string(),
    };
    eprintln!(
        "  {} {} {}",
        ">>>".blue().bold(),
        name.yellow(),
        summary.dimmed()
    );
}

/// Print tool result summary
pub fn print_tool_result(result: &str) {
    let lines: Vec<&str> = result.lines().collect();
    let preview = if lines.len() > 5 {
        format!(
            "{}\n  ... ({} lines total)",
            lines[..3].join("\n"),
            lines.len()
        )
    } else if result.len() > 300 {
        format!("{}... ({} chars)", &result[..300], result.len())
    } else {
        result.to_string()
    };
    eprintln!("  {} {}", "<<<".green().bold(), preview.dimmed());
}

/// Truncate output to reasonable limits (inspired by opencode: 2000 lines / 50KB)
pub(crate) fn truncate_output(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();

    if lines.len() > MAX_OUTPUT_LINES {
        let truncated: String = lines[..MAX_OUTPUT_LINES].join("\n");
        return format!(
            "{truncated}\n\n... Output truncated ({} lines shown of {}). Use read with offset/limit to see more.",
            MAX_OUTPUT_LINES,
            lines.len()
        );
    }

    if output.len() > MAX_OUTPUT_BYTES {
        let mut end = MAX_OUTPUT_BYTES;
        // Don't cut in the middle of a UTF-8 char
        while end > 0 && !output.is_char_boundary(end) {
            end -= 1;
        }
        return format!(
            "{}\n\n... Output truncated ({} bytes shown of {}). Use read with offset/limit to see more.",
            &output[..end],
            end,
            output.len()
        );
    }

    output.to_string()
}

// --- Tool Implementations ---

async fn exec_read(arguments: &str) -> Result<String> {
    let args: ReadArgs = serde_json::from_str(arguments)?;
    let content = tokio::fs::read_to_string(&args.path)
        .await
        .with_context(|| format!("reading {}", args.path))?;

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();
    let offset = args.offset.unwrap_or(1).max(1) as usize;
    let limit = args.limit.unwrap_or(DEFAULT_READ_LIMIT) as usize;

    // Convert to 0-indexed
    let start = (offset - 1).min(total_lines);
    let end = (start + limit).min(total_lines);

    let mut output = String::new();

    // Add line-numbered content
    for (i, line) in lines[start..end].iter().enumerate() {
        let line_num = start + i + 1;
        let truncated_line = if line.len() > MAX_CHARS_PER_LINE {
            format!("{}...", &line[..MAX_CHARS_PER_LINE])
        } else {
            line.to_string()
        };
        output.push_str(&format!("{line_num:>6}\t{truncated_line}\n"));
    }

    // Add range info
    if start > 0 || end < total_lines {
        output.push_str(&format!(
            "\nShowing lines {}-{} of {total_lines}",
            start + 1,
            end
        ));
    }

    Ok(output)
}

async fn exec_write(arguments: &str, session_id: &str) -> Result<String> {
    let args: WriteArgs = serde_json::from_str(arguments)?;

    // Save snapshot before overwriting
    save_file_snapshot(&args.path, session_id);

    // Create parent directories if needed
    if let Some(parent) = std::path::Path::new(&args.path).parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent)
                .await
                .context("creating directories")?;
        }
    }

    let len = args.content.len();
    let line_count = args.content.lines().count();
    tokio::fs::write(&args.path, &args.content)
        .await
        .with_context(|| format!("writing {}", args.path))?;

    Ok(format!(
        "Wrote {len} bytes ({line_count} lines) to {}",
        args.path
    ))
}

async fn exec_edit(arguments: &str, session_id: &str) -> Result<String> {
    let args: EditArgs = serde_json::from_str(arguments)?;

    ensure!(
        args.old_string != args.new_string,
        "old_string and new_string must be different"
    );

    // Save snapshot before editing
    save_file_snapshot(&args.path, session_id);

    let content = tokio::fs::read_to_string(&args.path)
        .await
        .with_context(|| format!("reading {}", args.path))?;

    let replace_all = args.replace_all.unwrap_or(false);
    let match_count = content.matches(&args.old_string).count();

    if match_count == 0 {
        // Try trimmed matching as fallback
        let trimmed_old = args.old_string.trim();
        let trimmed_count = content.matches(trimmed_old).count();
        if trimmed_count > 0 {
            bail!(
                "No exact match found, but found {trimmed_count} match(es) with trimmed whitespace. Check indentation/whitespace in old_string."
            );
        }
        bail!(
            "old_string not found in {}. Read the file first to see its current content.",
            args.path
        );
    }

    ensure!(
        match_count == 1 || replace_all,
        "Found {match_count} matches for old_string in {}. Provide more context to make it unique, or set replace_all=true.",
        args.path
    );

    let new_content = if replace_all {
        content.replace(&args.old_string, &args.new_string)
    } else {
        content.replacen(&args.old_string, &args.new_string, 1)
    };

    tokio::fs::write(&args.path, &new_content)
        .await
        .with_context(|| format!("writing {}", args.path))?;

    // Generate a simple diff summary
    let old_lines = args.old_string.lines().count();
    let new_lines = args.new_string.lines().count();
    let replacements = if replace_all { match_count } else { 1 };

    Ok(format!(
        "Edited {}: replaced {replacements} occurrence(s) ({old_lines} lines -> {new_lines} lines)",
        args.path
    ))
}

async fn exec_bash(arguments: &str) -> Result<String> {
    let args: BashArgs = serde_json::from_str(arguments)?;
    let timeout_secs = args.timeout.unwrap_or(120);

    let result = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&args.command)
            .output(),
    )
    .await
    .with_context(|| format!("Command timed out after {timeout_secs}s"))?
    .context("executing command")?;

    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    let exit = result.status.code().unwrap_or(-1);

    let mut output = format!("exit code: {exit}\n");
    if !stdout.is_empty() {
        output.push_str(&format!("stdout:\n{stdout}"));
    }
    if !stderr.is_empty() {
        output.push_str(&format!("stderr:\n{stderr}"));
    }

    Ok(output)
}

async fn exec_glob(arguments: &str) -> Result<String> {
    let args: GlobArgs = serde_json::from_str(arguments)?;
    let base_path = args.path.as_deref().unwrap_or(".");

    let result = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(format!(
            "find {} -path '{}' -not -path '*/target/*' -not -path '*/.git/*' -not -path '*/node_modules/*' 2>/dev/null | sort | head -200",
            shell_escape(base_path),
            shell_escape(&args.pattern)
        ))
        .output()
        .await
        .context("running glob")?;

    let output = String::from_utf8_lossy(&result.stdout).to_string();

    if output.trim().is_empty() {
        // Fallback: try with -name for simple patterns
        let result = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(format!(
                "find {} -name '{}' -not -path '*/target/*' -not -path '*/.git/*' -not -path '*/node_modules/*' 2>/dev/null | sort | head -200",
                shell_escape(base_path),
                shell_escape(&args.pattern)
            ))
            .output()
            .await
            .context("running glob")?;

        let output = String::from_utf8_lossy(&result.stdout).to_string();
        if output.trim().is_empty() {
            return Ok("No files found matching pattern".into());
        }
        let count = output.lines().count();
        return Ok(format!("{output}({count} files found)"));
    }

    let count = output.lines().count();
    Ok(format!("{output}({count} files found)"))
}

async fn exec_grep(arguments: &str) -> Result<String> {
    let args: GrepArgs = serde_json::from_str(arguments)?;
    let search_path = args.path.as_deref().unwrap_or(".");

    let mut cmd_args = vec!["-rn".to_string(), "--color=never".to_string()];

    // Exclude common noisy directories
    cmd_args.push("--exclude-dir=.git".into());
    cmd_args.push("--exclude-dir=target".into());
    cmd_args.push("--exclude-dir=node_modules".into());

    if let Some(include) = &args.include {
        cmd_args.push(format!("--include={include}"));
    }

    cmd_args.push(args.pattern.clone());
    cmd_args.push(search_path.to_string());

    let result = tokio::process::Command::new("grep")
        .args(&cmd_args)
        .output()
        .await
        .context("running grep")?;

    let output = String::from_utf8_lossy(&result.stdout).to_string();

    if output.is_empty() {
        return Ok("No matches found".into());
    }

    // Truncate long lines
    let processed: String = output
        .lines()
        .take(200)
        .map(|line| {
            if line.len() > MAX_CHARS_PER_LINE {
                format!("{}...\n", &line[..MAX_CHARS_PER_LINE])
            } else {
                format!("{line}\n")
            }
        })
        .collect();

    let total_matches = output.lines().count();
    if total_matches > 200 {
        Ok(format!(
            "{processed}\n... Showing 200 of {total_matches} matches. Narrow your search pattern."
        ))
    } else {
        Ok(format!("{processed}({total_matches} matches)"))
    }
}

async fn exec_list_files(arguments: &str) -> Result<String> {
    let args: ListFilesArgs = serde_json::from_str(arguments)?;

    let mut entries = Vec::new();
    let mut dir = tokio::fs::read_dir(&args.path)
        .await
        .with_context(|| format!("reading {}", args.path))?;

    while let Some(entry) = dir.next_entry().await? {
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry
            .file_type()
            .await
            .map(|ft| ft.is_dir())
            .unwrap_or(false);
        if is_dir {
            entries.push(format!("{name}/"));
        } else {
            // Show file size
            let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
            entries.push(format!("{name}  ({size} bytes)"));
        }
    }

    entries.sort();
    Ok(entries.join("\n"))
}

// --- Memory Tools ---

async fn exec_memory_save(arguments: &str) -> Result<String> {
    let args: MemorySaveArgs = serde_json::from_str(arguments)?;

    let memory = crate::types::ContextMemory {
        name: args.name.clone(),
        description: args.description,
        memory_type: args.memory_type,
        content: args.content,
    };

    // Check for optional folder field
    let folder: Option<String> = serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|v| v.get("folder")?.as_str().map(|s| s.to_string()));

    let path = if let Some(ref folder) = folder {
        crate::persistence::save_memory_to(&memory, folder)?
    } else {
        crate::persistence::save_memory(&memory)?
    };

    Ok(format!(
        "Memory '{}' saved to {}",
        args.name,
        path.display()
    ))
}

async fn exec_memory_delete(arguments: &str) -> Result<String> {
    let args: MemoryDeleteArgs = serde_json::from_str(arguments)?;

    match crate::persistence::delete_memory(&args.name)? {
        true => Ok(format!("Deleted memory '{}'.", args.name)),
        false => Ok(format!("Memory '{}' not found.", args.name)),
    }
}

async fn exec_memory_list() -> Result<String> {
    let memories = crate::persistence::list_memories();
    if memories.is_empty() {
        return Ok("No memories saved.".to_string());
    }

    let mut output = String::from("Saved memories:\n");
    for (name, desc, mtype, size) in &memories {
        let desc_part = if desc.is_empty() {
            String::new()
        } else {
            format!(" — {desc}")
        };
        output.push_str(&format!("  - {name} [{mtype}] ({size} bytes){desc_part}\n"));
    }
    Ok(output)
}

// --- Protected Files ---

/// Check if a tool is trying to modify a protected file. Returns error message if blocked.
fn check_protected_file(tool_name: &str, arguments: &str) -> Option<String> {
    let path = match tool_name {
        "write" | "edit" => serde_json::from_str::<serde_json::Value>(arguments)
            .ok()
            .and_then(|v| v.get("path")?.as_str().map(|s| s.to_string())),
        "apply_patch" => {
            // Check all files in the patch
            let v = serde_json::from_str::<serde_json::Value>(arguments).ok()?;
            let patch = v.get("patch")?.as_str()?;
            for line in patch.lines() {
                if line.starts_with("+++ ") {
                    let file_path = line.trim_start_matches("+++ ").trim_start_matches("b/");
                    if is_protected_path(file_path) {
                        return Some(format!(
                            "Error: Refusing to modify protected file '{}'. \
                             This file may contain secrets or credentials. \
                             If you need to modify it, ask the user to do so manually.",
                            file_path
                        ));
                    }
                }
            }
            return None;
        }
        _ => return None,
    };

    if let Some(ref path) = path {
        if is_protected_path(path) {
            return Some(format!(
                "Error: Refusing to modify protected file '{}'. \
                 This file may contain secrets or credentials. \
                 If you need to modify it, ask the user to do so manually.",
                path
            ));
        }
    }
    None
}

/// Check if a file path matches any protected pattern
fn is_protected_path(path: &str) -> bool {
    let filename = std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    for pattern in crate::types::PROTECTED_FILE_PATTERNS {
        // Match by filename
        if filename == *pattern {
            return true;
        }
        // Match by extension (for .pem, .key, etc.)
        if pattern.starts_with('.') && filename.ends_with(pattern) {
            return true;
        }
        // Match if the path contains the pattern as a component
        if path.ends_with(pattern) {
            return true;
        }
    }
    false
}

// --- Web Fetch Tool ---

async fn exec_webfetch(arguments: &str) -> Result<String> {
    let args: WebFetchArgs = serde_json::from_str(arguments)?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("Mozilla/5.0 (compatible; bfcode/0.6.0; +https://github.com/user/bfcode)")
        .build()?;

    let response = client
        .get(&args.url)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", "en-US,en;q=0.5")
        .send()
        .await
        .with_context(|| format!("Failed to fetch {}", args.url))?;

    let status = response.status();
    if !status.is_success() {
        return Ok(format!(
            "HTTP {} {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or("")
        ));
    }

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let body = response.text().await?;

    // If HTML, strip tags to get text content
    let text = if content_type.contains("html") {
        strip_html_tags(&body)
    } else {
        body
    };

    // Truncate to avoid flooding context
    let max_chars = 50_000;
    if text.len() > max_chars {
        Ok(format!(
            "{}\n\n[Truncated — {} chars total, showing first {}]",
            &text[..max_chars],
            text.len(),
            max_chars
        ))
    } else {
        Ok(text)
    }
}

/// Simple HTML tag stripping — removes tags, decodes common entities, collapses whitespace
fn strip_html_tags(html: &str) -> String {
    let mut text = html.to_string();

    // Remove script and style blocks (case insensitive)
    loop {
        let lower = text.to_lowercase();
        if let Some(start) = lower.find("<script") {
            if let Some(end) = lower[start..].find("</script") {
                if let Some(close) = lower[start + end..].find('>') {
                    text = format!("{}{}", &text[..start], &text[start + end + close + 1..]);
                    continue;
                }
            }
            // Malformed — remove from <script to end
            text = text[..start].to_string();
        }
        break;
    }
    loop {
        let lower = text.to_lowercase();
        if let Some(start) = lower.find("<style") {
            if let Some(end) = lower[start..].find("</style") {
                if let Some(close) = lower[start + end..].find('>') {
                    text = format!("{}{}", &text[..start], &text[start + end + close + 1..]);
                    continue;
                }
            }
            text = text[..start].to_string();
        }
        break;
    }

    // Remove all HTML tags
    let mut result = String::with_capacity(text.len());
    let mut in_tag = false;
    for ch in text.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            result.push(ch);
        }
    }

    // Decode common HTML entities
    let result = result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ");

    // Collapse whitespace
    let mut collapsed = String::with_capacity(result.len());
    let mut prev_whitespace = false;
    for ch in result.chars() {
        if ch.is_whitespace() {
            if !prev_whitespace {
                collapsed.push(if ch == '\n' { '\n' } else { ' ' });
            }
            prev_whitespace = true;
        } else {
            collapsed.push(ch);
            prev_whitespace = false;
        }
    }

    collapsed.trim().to_string()
}

// --- File Snapshot Helper ---

/// Save a snapshot of a file before modification (for undo support)
fn save_file_snapshot(path: &str, session_id: &str) {
    if std::path::Path::new(path).exists() {
        if let Ok(original) = std::fs::read_to_string(path) {
            let snapshot = crate::types::FileSnapshot {
                path: path.to_string(),
                original_content: original,
                timestamp: chrono::Local::now().format("%Y%m%d_%H%M%S_%3f").to_string(),
                message_index: 0,
            };
            let _ = crate::persistence::save_snapshot(session_id, &snapshot);
        }
    }
}

// --- Apply Patch Tool ---

async fn exec_apply_patch(arguments: &str, session_id: &str) -> Result<String> {
    let args: ApplyPatchArgs = serde_json::from_str(arguments)?;
    let file_patches = parse_unified_diff(&args.patch)?;

    if file_patches.is_empty() {
        bail!("No valid patches found in input");
    }

    let mut results = Vec::new();

    for fp in &file_patches {
        // Save snapshot before modifying
        save_file_snapshot(&fp.target_path, session_id);

        let content = if std::path::Path::new(&fp.target_path).exists() {
            tokio::fs::read_to_string(&fp.target_path)
                .await
                .unwrap_or_default()
        } else {
            String::new()
        };

        let patched = apply_hunks(&content, &fp.hunks)?;

        // Create parent dirs
        if let Some(parent) = std::path::Path::new(&fp.target_path).parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await?;
            }
        }

        tokio::fs::write(&fp.target_path, &patched)
            .await
            .with_context(|| format!("writing {}", fp.target_path))?;

        let status = if content.is_empty() { "A" } else { "M" };
        results.push(format!(
            "{status} {} ({} hunks applied)",
            fp.target_path,
            fp.hunks.len()
        ));
    }

    Ok(results.join("\n"))
}

// --- Unified Diff Parser ---

struct FilePatch {
    target_path: String,
    hunks: Vec<Hunk>,
}

struct Hunk {
    old_start: usize,
    _old_count: usize,
    _new_start: usize,
    _new_count: usize,
    lines: Vec<DiffLine>,
}

#[derive(Debug)]
enum DiffLine {
    Context(String),
    Add(String),
    Remove(String),
}

fn parse_unified_diff(patch: &str) -> Result<Vec<FilePatch>> {
    let mut file_patches = Vec::new();
    let lines: Vec<&str> = patch.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        // Look for --- header
        if lines[i].starts_with("--- ") && i + 1 < lines.len() && lines[i + 1].starts_with("+++ ") {
            let _old_path = lines[i].strip_prefix("--- ").unwrap_or("").trim();
            let new_path = lines[i + 1].strip_prefix("+++ ").unwrap_or("").trim();

            // Strip a/ b/ prefixes
            let target = new_path
                .strip_prefix("b/")
                .or_else(|| new_path.strip_prefix("a/"))
                .unwrap_or(new_path)
                .to_string();

            i += 2;

            let mut hunks = Vec::new();

            // Parse hunks
            while i < lines.len() && !lines[i].starts_with("--- ") {
                if lines[i].starts_with("@@ ") {
                    // Parse @@ -old_start,old_count +new_start,new_count @@
                    let hunk_header = lines[i];
                    let (old_start, old_count, new_start, new_count) =
                        parse_hunk_header(hunk_header)?;
                    i += 1;

                    let mut hunk_lines = Vec::new();
                    while i < lines.len()
                        && !lines[i].starts_with("@@ ")
                        && !lines[i].starts_with("--- ")
                    {
                        let line = lines[i];
                        if line.starts_with('+') {
                            hunk_lines.push(DiffLine::Add(line[1..].to_string()));
                        } else if line.starts_with('-') {
                            hunk_lines.push(DiffLine::Remove(line[1..].to_string()));
                        } else if line.starts_with(' ') {
                            hunk_lines.push(DiffLine::Context(line[1..].to_string()));
                        } else if line == "\\ No newline at end of file" {
                            // Skip
                        } else if line.is_empty() {
                            // Empty context line
                            hunk_lines.push(DiffLine::Context(String::new()));
                        } else {
                            break;
                        }
                        i += 1;
                    }

                    hunks.push(Hunk {
                        old_start,
                        _old_count: old_count,
                        _new_start: new_start,
                        _new_count: new_count,
                        lines: hunk_lines,
                    });
                } else {
                    i += 1;
                }
            }

            if !hunks.is_empty() || target != "/dev/null" {
                file_patches.push(FilePatch {
                    target_path: target,
                    hunks,
                });
            }
        } else {
            i += 1;
        }
    }

    Ok(file_patches)
}

fn parse_hunk_header(header: &str) -> Result<(usize, usize, usize, usize)> {
    // @@ -1,5 +1,7 @@ optional context
    let parts: Vec<&str> = header.split("@@").collect();
    if parts.len() < 2 {
        bail!("Invalid hunk header: {header}");
    }
    let range_part = parts[1].trim();
    let ranges: Vec<&str> = range_part.split_whitespace().collect();
    if ranges.len() < 2 {
        bail!("Invalid hunk ranges: {range_part}");
    }

    let old = parse_range(ranges[0].strip_prefix('-').unwrap_or(ranges[0]))?;
    let new = parse_range(ranges[1].strip_prefix('+').unwrap_or(ranges[1]))?;

    Ok((old.0, old.1, new.0, new.1))
}

fn parse_range(s: &str) -> Result<(usize, usize)> {
    if let Some((start, count)) = s.split_once(',') {
        Ok((start.parse()?, count.parse()?))
    } else {
        let start: usize = s.parse()?;
        Ok((start, 1))
    }
}

fn apply_hunks(content: &str, hunks: &[Hunk]) -> Result<String> {
    let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();

    // Apply hunks in reverse order to preserve line numbers
    let mut sorted_hunks: Vec<&Hunk> = hunks.iter().collect();
    sorted_hunks.sort_by(|a, b| b.old_start.cmp(&a.old_start));

    for hunk in sorted_hunks {
        let start_idx = if hunk.old_start == 0 {
            0
        } else {
            hunk.old_start - 1
        };

        let mut remove_count = 0;
        let mut add_lines = Vec::new();
        let mut pos = start_idx;

        for diff_line in &hunk.lines {
            match diff_line {
                DiffLine::Context(_) => {
                    pos += 1;
                }
                DiffLine::Remove(_) => {
                    remove_count += 1;
                    pos += 1;
                }
                DiffLine::Add(text) => {
                    add_lines.push((pos - remove_count, text.clone()));
                }
            }
        }

        // Simpler approach: rebuild the affected region
        let mut new_lines = Vec::new();
        let mut src_pos = start_idx;

        for diff_line in &hunk.lines {
            match diff_line {
                DiffLine::Context(text) => {
                    new_lines.push(text.clone());
                    src_pos += 1;
                }
                DiffLine::Remove(_) => {
                    src_pos += 1;
                }
                DiffLine::Add(text) => {
                    new_lines.push(text.clone());
                }
            }
        }

        // Count context + remove lines to know how many old lines to replace
        let old_line_count = hunk
            .lines
            .iter()
            .filter(|l| !matches!(l, DiffLine::Add(_)))
            .count();
        let end_idx = (start_idx + old_line_count).min(lines.len());

        // Replace the range
        lines.splice(start_idx..end_idx, new_lines);
    }

    let mut result = lines.join("\n");
    // Preserve trailing newline if original had one
    if content.ends_with('\n') && !result.ends_with('\n') {
        result.push('\n');
    }
    Ok(result)
}

// --- Web Search Tool ---

async fn exec_websearch(arguments: &str) -> Result<String> {
    let args: WebSearchArgs = serde_json::from_str(arguments)?;
    let num_results = args.num_results.unwrap_or(5);

    // Try Brave Search first, then Tavily
    if let Ok(api_key) = std::env::var("BRAVE_API_KEY") {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?;

        let resp = client
            .get("https://api.search.brave.com/res/v1/web/search")
            .header("Accept", "application/json")
            .header("Accept-Encoding", "gzip")
            .header("X-Subscription-Token", &api_key)
            .query(&[("q", &args.query), ("count", &num_results.to_string())])
            .send()
            .await
            .context("Brave Search API request failed")?;

        let body: serde_json::Value = resp.json().await?;
        let mut output = format!("Web search results for: \"{}\"\n\n", args.query);

        if let Some(results) = body["web"]["results"].as_array() {
            for (i, result) in results.iter().enumerate() {
                let title = result["title"].as_str().unwrap_or("Untitled");
                let url = result["url"].as_str().unwrap_or("");
                let desc = result["description"].as_str().unwrap_or("");
                output.push_str(&format!(
                    "{}. {}\n   {}\n   {}\n\n",
                    i + 1,
                    title,
                    url,
                    desc
                ));
            }
            if results.is_empty() {
                output.push_str("No results found.\n");
            }
        } else {
            output.push_str("No results found.\n");
        }
        return Ok(output);
    }

    if let Ok(api_key) = std::env::var("TAVILY_API_KEY") {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?;

        let body = serde_json::json!({
            "api_key": api_key,
            "query": args.query,
            "max_results": num_results,
            "include_answer": true,
        });

        let resp = client
            .post("https://api.tavily.com/search")
            .json(&body)
            .send()
            .await
            .context("Tavily API request failed")?;

        let result: serde_json::Value = resp.json().await?;
        let mut output = format!("Web search results for: \"{}\"\n\n", args.query);

        if let Some(answer) = result["answer"].as_str() {
            if !answer.is_empty() {
                output.push_str(&format!("Summary: {}\n\n", answer));
            }
        }

        if let Some(results) = result["results"].as_array() {
            for (i, r) in results.iter().enumerate() {
                let title = r["title"].as_str().unwrap_or("Untitled");
                let url = r["url"].as_str().unwrap_or("");
                let content = r["content"].as_str().unwrap_or("");
                let snippet: String = content.chars().take(200).collect();
                output.push_str(&format!(
                    "{}. {}\n   {}\n   {}\n\n",
                    i + 1,
                    title,
                    url,
                    snippet
                ));
            }
        }
        return Ok(output);
    }

    bail!("No search API key found. Set BRAVE_API_KEY or TAVILY_API_KEY environment variable.")
}

// --- Memory Search Tool ---

async fn exec_memory_search(arguments: &str) -> Result<String> {
    let args: MemorySearchArgs = serde_json::from_str(arguments)?;
    let top_k = args.top_k.unwrap_or(5);

    let memories = crate::persistence::list_memories();
    if memories.is_empty() {
        return Ok("No memories saved to search.".to_string());
    }

    // Load all memory contents
    let mut docs = Vec::new();
    for (name, desc, _, _) in &memories {
        if let Some(mem) = crate::persistence::load_memory(name) {
            docs.push((name.clone(), format!("{}\n{}\n{}", name, desc, mem.content)));
        }
    }

    let index = crate::search::TfidfIndex::build(docs);
    let results = index.search(&args.query, top_k);

    if results.is_empty() {
        return Ok(format!(
            "No relevant memories found for: \"{}\"",
            args.query
        ));
    }

    let mut output = format!("Memory search results for: \"{}\"\n\n", args.query);
    for (i, r) in results.iter().enumerate() {
        output.push_str(&format!(
            "{}. {} (score: {:.3})\n   {}\n\n",
            i + 1,
            r.name,
            r.score,
            r.snippet
        ));
    }
    Ok(output)
}

// --- PDF Read Tool ---

async fn exec_pdf_read(arguments: &str) -> Result<String> {
    let args: PdfReadArgs = serde_json::from_str(arguments)?;

    let bytes = tokio::fs::read(&args.path)
        .await
        .with_context(|| format!("reading PDF {}", args.path))?;

    let text = pdf_extract::extract_text_from_mem(&bytes)
        .with_context(|| format!("extracting text from PDF {}", args.path))?;

    // If pages specified, filter
    if let Some(ref pages_str) = args.pages {
        let pages: Vec<&str> = text.split('\u{0c}').collect(); // form feed = page break
        let total_pages = pages.len();
        let (start, end) = parse_page_range(pages_str, total_pages)?;

        let mut output = String::new();
        for i in start..=end.min(total_pages - 1) {
            output.push_str(&format!("--- Page {} ---\n", i + 1));
            output.push_str(pages.get(i).unwrap_or(&""));
            output.push('\n');
        }
        output.push_str(&format!(
            "\nShowing pages {}-{} of {}",
            start + 1,
            end.min(total_pages - 1) + 1,
            total_pages
        ));
        Ok(output)
    } else {
        let pages: Vec<&str> = text.split('\u{0c}').collect();
        let mut output = String::new();
        for (i, page) in pages.iter().enumerate() {
            if !page.trim().is_empty() {
                output.push_str(&format!("--- Page {} ---\n", i + 1));
                output.push_str(page);
                output.push('\n');
            }
        }
        Ok(output)
    }
}

fn parse_page_range(s: &str, total: usize) -> Result<(usize, usize)> {
    if let Some((start_s, end_s)) = s.split_once('-') {
        let start: usize = start_s.trim().parse::<usize>()?.saturating_sub(1);
        let end: usize = end_s.trim().parse::<usize>()?.saturating_sub(1);
        Ok((start.min(total - 1), end.min(total - 1)))
    } else {
        let page: usize = s.trim().parse::<usize>()?.saturating_sub(1);
        Ok((page.min(total - 1), page.min(total - 1)))
    }
}

// --- Image Generation Tool ---

async fn exec_image_generate(arguments: &str) -> Result<String> {
    let args: ImageGenerateArgs = serde_json::from_str(arguments)?;

    let api_key = std::env::var("OPENAI_API_KEY")
        .context("OPENAI_API_KEY environment variable not set. Required for image generation.")?;

    let size = args.size.as_deref().unwrap_or("1024x1024");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;

    let body = serde_json::json!({
        "model": "dall-e-3",
        "prompt": args.prompt,
        "n": 1,
        "size": size,
        "response_format": "b64_json"
    });

    let resp = client
        .post("https://api.openai.com/v1/images/generations")
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&body)
        .send()
        .await
        .context("DALL-E API request failed")?;

    let status = resp.status();
    let result: serde_json::Value = resp.json().await?;

    if !status.is_success() {
        let err_msg = result["error"]["message"]
            .as_str()
            .unwrap_or("Unknown error");
        bail!("DALL-E API error: {err_msg}");
    }

    let b64_data = result["data"][0]["b64_json"]
        .as_str()
        .context("No image data in response")?;

    let image_bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64_data)?;

    let output_path = match args.output_path {
        Some(p) => p,
        None => {
            let dir = ".bfcode/generated";
            tokio::fs::create_dir_all(dir).await?;
            let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
            format!("{dir}/{ts}.png")
        }
    };

    tokio::fs::write(&output_path, &image_bytes)
        .await
        .with_context(|| format!("writing image to {output_path}"))?;

    let revised_prompt = result["data"][0]["revised_prompt"]
        .as_str()
        .unwrap_or(&args.prompt);

    Ok(format!(
        "Image generated and saved to: {output_path}\nSize: {size}\nPrompt: {revised_prompt}"
    ))
}

// --- TTS Tool ---

async fn exec_tts(arguments: &str) -> Result<String> {
    let args: TtsArgs = serde_json::from_str(arguments)?;

    // If output_path is provided or OPENAI_API_KEY is set, try API first
    if let Ok(api_key) = std::env::var("OPENAI_API_KEY") {
        if args.output_path.is_some()
            || args
                .voice
                .as_deref()
                .map(|v| matches!(v, "alloy" | "echo" | "fable" | "onyx" | "nova" | "shimmer"))
                .unwrap_or(false)
        {
            let voice = args.voice.as_deref().unwrap_or("alloy");
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()?;

            let body = serde_json::json!({
                "model": "tts-1",
                "input": args.text,
                "voice": voice,
            });

            let resp = client
                .post("https://api.openai.com/v1/audio/speech")
                .header("Authorization", format!("Bearer {api_key}"))
                .json(&body)
                .send()
                .await
                .context("OpenAI TTS API request failed")?;

            if !resp.status().is_success() {
                let err = resp.text().await.unwrap_or_default();
                bail!("TTS API error: {err}");
            }

            let audio_bytes = resp.bytes().await?;
            let output_path = args.output_path.unwrap_or_else(|| {
                let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
                format!(".bfcode/generated/{ts}.mp3")
            });

            if let Some(parent) = std::path::Path::new(&output_path).parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&output_path, &audio_bytes).await?;

            return Ok(format!(
                "Audio saved to: {output_path} ({} bytes, voice: {voice})",
                audio_bytes.len()
            ));
        }
    }

    // Fall back to system TTS
    let voice_arg = args.voice.as_deref().unwrap_or("");

    if cfg!(target_os = "macos") {
        let mut cmd = vec!["say".to_string()];
        if !voice_arg.is_empty() {
            cmd.push("-v".into());
            cmd.push(voice_arg.into());
        }
        if let Some(ref path) = args.output_path {
            cmd.push("-o".into());
            cmd.push(path.clone());
        }
        cmd.push(args.text.clone());

        let result = tokio::process::Command::new(&cmd[0])
            .args(&cmd[1..])
            .output()
            .await
            .context("Failed to run 'say' command")?;

        if result.status.success() {
            if let Some(ref path) = args.output_path {
                Ok(format!("Speech saved to: {path}"))
            } else {
                Ok("Text spoken via system TTS.".to_string())
            }
        } else {
            let stderr = String::from_utf8_lossy(&result.stderr);
            bail!("TTS failed: {stderr}");
        }
    } else {
        // Linux: try espeak
        let mut cmd = vec!["espeak".to_string()];
        if let Some(ref path) = args.output_path {
            cmd.push("-w".into());
            cmd.push(path.clone());
        }
        cmd.push(args.text.clone());

        let result = tokio::process::Command::new(&cmd[0])
            .args(&cmd[1..])
            .output()
            .await
            .context("Failed to run 'espeak'. Install it with: sudo apt install espeak")?;

        if result.status.success() {
            if let Some(ref path) = args.output_path {
                Ok(format!("Speech saved to: {path}"))
            } else {
                Ok("Text spoken via espeak.".to_string())
            }
        } else {
            let stderr = String::from_utf8_lossy(&result.stderr);
            bail!("TTS failed: {stderr}");
        }
    }
}

// --- Browser Tools ---

async fn exec_browser_navigate(arguments: &str) -> Result<String> {
    let args: BrowserNavigateArgs = serde_json::from_str(arguments)?;
    crate::browser::browser_navigate(&args.url).await
}

async fn exec_browser_screenshot(arguments: &str) -> Result<String> {
    let args: BrowserScreenshotArgs = serde_json::from_str(arguments)?;
    crate::browser::browser_screenshot(args.output_path.as_deref()).await
}

async fn exec_browser_click(arguments: &str) -> Result<String> {
    let args: BrowserClickArgs = serde_json::from_str(arguments)?;
    crate::browser::browser_click(&args.selector).await
}

async fn exec_browser_type(arguments: &str) -> Result<String> {
    let args: BrowserTypeArgs = serde_json::from_str(arguments)?;
    crate::browser::browser_type(&args.selector, &args.text).await
}

async fn exec_browser_evaluate(arguments: &str) -> Result<String> {
    let args: BrowserEvaluateArgs = serde_json::from_str(arguments)?;
    crate::browser::browser_evaluate(&args.script).await
}

async fn exec_browser_close() -> Result<String> {
    crate::browser::browser_close().await
}

// --- Multi-Edit Tool ---

async fn exec_multiedit(arguments: &str, session_id: &str) -> Result<String> {
    let args: MultiEditArgs = serde_json::from_str(arguments)?;

    ensure!(!args.edits.is_empty(), "edits array must not be empty");

    // Save snapshot before editing
    save_file_snapshot(&args.path, session_id);

    let mut content = tokio::fs::read_to_string(&args.path)
        .await
        .with_context(|| format!("reading {}", args.path))?;

    let mut total_replacements = 0;

    for (i, edit) in args.edits.iter().enumerate() {
        ensure!(
            edit.old_string != edit.new_string,
            "Edit #{}: old_string and new_string must be different",
            i + 1
        );

        let replace_all = edit.replace_all.unwrap_or(false);
        let match_count = content.matches(&edit.old_string).count();

        if match_count == 0 {
            let trimmed_old = edit.old_string.trim();
            let trimmed_count = content.matches(trimmed_old).count();
            if trimmed_count > 0 {
                bail!(
                    "Edit #{}: No exact match found, but found {trimmed_count} match(es) with trimmed whitespace. Check indentation.",
                    i + 1
                );
            }
            bail!(
                "Edit #{}: old_string not found in {}. Previous edits may have changed the content.",
                i + 1,
                args.path
            );
        }

        ensure!(
            match_count == 1 || replace_all,
            "Edit #{}: Found {match_count} matches. Provide more context or set replace_all=true.",
            i + 1
        );

        content = if replace_all {
            content.replace(&edit.old_string, &edit.new_string)
        } else {
            content.replacen(&edit.old_string, &edit.new_string, 1)
        };

        total_replacements += if replace_all { match_count } else { 1 };
    }

    tokio::fs::write(&args.path, &content)
        .await
        .with_context(|| format!("writing {}", args.path))?;

    Ok(format!(
        "Multi-edited {}: applied {} edits ({total_replacements} total replacements)",
        args.path,
        args.edits.len()
    ))
}

// --- Batch Tool ---

const MAX_BATCH_CALLS: usize = 25;
const DISALLOWED_BATCH_TOOLS: &[&str] = &["batch", "task"]; // prevent nesting

async fn exec_batch(
    arguments: &str,
    permissions: &Permissions,
    session_id: &str,
) -> Result<String> {
    let args: BatchArgs = serde_json::from_str(arguments)?;

    ensure!(
        !args.tool_calls.is_empty(),
        "tool_calls array must not be empty"
    );

    let call_count = args.tool_calls.len();
    if call_count > MAX_BATCH_CALLS {
        bail!("Too many tool calls ({call_count}). Maximum is {MAX_BATCH_CALLS}.");
    }

    // Validate tools
    for (i, tc) in args.tool_calls.iter().enumerate() {
        if DISALLOWED_BATCH_TOOLS.contains(&tc.tool.as_str()) {
            bail!(
                "Tool call #{}: '{}' cannot be used inside batch.",
                i + 1,
                tc.tool
            );
        }
    }

    // Execute all tool calls in parallel using futures
    let futures: Vec<_> = args
        .tool_calls
        .iter()
        .map(|tc| {
            let tool_args = serde_json::to_string(&tc.parameters).unwrap_or_default();
            let tool_name = tc.tool.clone();
            async move {
                let result = execute_tool(&tool_name, &tool_args, permissions, session_id).await;
                (tool_name, result)
            }
        })
        .collect();

    let results: Vec<(String, String)> = futures_util::future::join_all(futures).await;

    Ok(format!(
        "Batch executed {} tool calls:\n\n{}",
        call_count,
        results
            .iter()
            .enumerate()
            .map(|(i, (name, output))| format!("--- Call {} [{name}] ---\n{output}", i + 1))
            .collect::<Vec<_>>()
            .join("\n\n")
    ))
}

// --- Task/Subagent Tool ---

async fn exec_task(arguments: &str, permissions: &Permissions, session_id: &str) -> Result<String> {
    let args: TaskToolArgs = serde_json::from_str(arguments)?;
    let agent_type = args.subagent_type.as_deref().unwrap_or("explore");

    // Load agent definitions and find matching agent
    let agents = crate::agent::load_agents();
    let agent_def = crate::agent::find_agent(&agents, agent_type);

    eprintln!(
        "  {} Spawning {} subagent: {}",
        "+".green(),
        agent_type.cyan(),
        args.description.dimmed()
    );

    // Get tool list from agent definition, or fall back to defaults
    let restricted_tools: Vec<String> = if let Some(def) = &agent_def {
        if def.tools.is_empty() {
            // Default tools for unknown agents
            vec![
                "read",
                "glob",
                "grep",
                "list_files",
                "webfetch",
                "websearch",
                "memory_list",
                "memory_search",
            ]
            .into_iter()
            .map(String::from)
            .collect()
        } else {
            def.tools.clone()
        }
    } else {
        // Fallback for unknown agent types
        match agent_type {
            "explore" => vec![
                "read",
                "glob",
                "grep",
                "list_files",
                "webfetch",
                "websearch",
                "memory_list",
                "memory_search",
                "pdf_read",
            ],
            "plan" => vec![
                "read",
                "glob",
                "grep",
                "list_files",
                "webfetch",
                "websearch",
                "memory_list",
                "memory_search",
                "pdf_read",
                "write",
            ],
            "build" => vec![
                "read",
                "write",
                "edit",
                "bash",
                "glob",
                "grep",
                "list_files",
                "apply_patch",
                "multiedit",
                "webfetch",
                "websearch",
                "memory_save",
                "memory_list",
                "memory_search",
            ],
            _ => vec![
                "read",
                "glob",
                "grep",
                "list_files",
                "webfetch",
                "websearch",
                "memory_list",
                "memory_search",
            ],
        }
        .into_iter()
        .map(String::from)
        .collect()
    };

    // Get agent system prompt
    let agent_prompt = agent_def
        .as_ref()
        .map(|d| d.prompt.as_str())
        .unwrap_or("You are a subagent working on a specific task.");

    // Get max rounds from agent definition
    let max_rounds = agent_def.as_ref().map(|d| d.max_rounds).unwrap_or(15);

    // Create a child session
    let child_session_id = format!(
        "{session_id}_task_{}",
        chrono::Local::now().format("%H%M%S")
    );

    // Build system prompt for subagent
    let subagent_prompt = format!(
        "{agent_prompt}\n\n\
         Your task: {}\n\n\
         Instructions:\n\
         {}\n\n\
         Available tools: {}\n\
         Complete the task and provide a clear summary of your findings/results.",
        args.description,
        args.prompt,
        restricted_tools.join(", ")
    );

    // Get the available tool definitions, filtered by allowed tools
    let all_tools = get_tool_definitions();
    let filtered_tools: Vec<ToolDefinition> = all_tools
        .into_iter()
        .filter(|t| restricted_tools.contains(&t.function.name))
        .collect();

    // Create the subagent conversation
    let messages = vec![
        Message::system(&subagent_prompt),
        Message::user(&args.prompt),
    ];

    // Load config and create client (may use agent's model override)
    let mut config = crate::persistence::load_config();
    if let Some(ref def) = agent_def {
        if let Some(ref model) = def.model {
            config.model = model.clone();
        }
    }
    let client = crate::api::create_client(&config)?;

    // Run the subagent loop
    let mut conversation = messages;
    let mut final_response = String::from("(no response from subagent)");

    for _round in 0..max_rounds {
        let response = client
            .chat(
                &conversation,
                &filtered_tools,
                &config.model,
                config.temperature,
            )
            .await?;

        if response.choices.is_empty() {
            break;
        }

        let assistant_msg = &response.choices[0].message;

        if let Some(tool_calls) = &assistant_msg.tool_calls {
            conversation.push(Message::assistant_tool_calls(tool_calls.clone()));

            for tc in tool_calls {
                // Only allow tools in the restricted set
                if !restricted_tools.contains(&tc.function.name) {
                    conversation.push(Message::tool_result(
                        &tc.id,
                        &format!(
                            "Error: Tool '{}' is not available for {} subagent.",
                            tc.function.name, agent_type
                        ),
                    ));
                    continue;
                }

                let result = execute_tool(
                    &tc.function.name,
                    &tc.function.arguments,
                    permissions,
                    &child_session_id,
                )
                .await;
                conversation.push(Message::tool_result(&tc.id, &result));
            }
            continue;
        }

        if let Some(content) = &assistant_msg.content {
            final_response = content.clone();
            conversation.push(Message::assistant_text(content));
        }
        break;
    }

    Ok(format!(
        "<task_result>\nAgent: {agent_type}\nTask: {}\n\n{final_response}\n</task_result>",
        args.description
    ))
}

// --- Todo Tools ---

async fn exec_todowrite(arguments: &str, session_id: &str) -> Result<String> {
    let args: TodoWriteArgs = serde_json::from_str(arguments)?;

    let count = args.todos.len();
    let mut todos = SESSION_TODOS.lock().unwrap_or_else(|e| e.into_inner());
    todos.insert(session_id.to_string(), args.todos);

    // Also persist to .bfcode/sessions/{session_id}_todos.json
    let todo_path = format!(".bfcode/sessions/{session_id}_todos.json");
    if let Some(parent) = std::path::Path::new(&todo_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(todos.get(session_id).unwrap()) {
        let _ = std::fs::write(&todo_path, json);
    }

    Ok(format!("Todo list updated: {count} items"))
}

async fn exec_todoread(session_id: &str) -> Result<String> {
    let todos = SESSION_TODOS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(items) = todos.get(session_id) {
        if !items.is_empty() {
            return Ok(format_todos(items));
        }
    }
    drop(todos);

    // Try loading from disk
    let todo_path = format!(".bfcode/sessions/{session_id}_todos.json");
    if let Ok(data) = std::fs::read_to_string(&todo_path) {
        if let Ok(items) = serde_json::from_str::<Vec<TodoItem>>(&data) {
            if !items.is_empty() {
                let mut todos = SESSION_TODOS.lock().unwrap_or_else(|e| e.into_inner());
                todos.insert(session_id.to_string(), items.clone());
                return Ok(format_todos(&items));
            }
        }
    }
    Ok("No todos in this session.".to_string())
}

fn format_todos(items: &[TodoItem]) -> String {
    let mut output = String::from("Session todos:\n");
    for (i, item) in items.iter().enumerate() {
        let icon = match item.status {
            TodoStatus::Pending => "[ ]",
            TodoStatus::InProgress => "[~]",
            TodoStatus::Completed => "[x]",
            TodoStatus::Cancelled => "[-]",
        };
        let priority_tag = match item.priority {
            TodoPriority::High => " [HIGH]",
            TodoPriority::Medium => "",
            TodoPriority::Low => " [low]",
        };
        output.push_str(&format!(
            "  {}. {} {}{}\n",
            i + 1,
            icon,
            item.content,
            priority_tag
        ));
    }

    // Summary
    let pending = items
        .iter()
        .filter(|t| t.status == TodoStatus::Pending)
        .count();
    let in_progress = items
        .iter()
        .filter(|t| t.status == TodoStatus::InProgress)
        .count();
    let completed = items
        .iter()
        .filter(|t| t.status == TodoStatus::Completed)
        .count();
    output.push_str(&format!(
        "\nSummary: {} pending, {} in progress, {} completed (of {} total)",
        pending,
        in_progress,
        completed,
        items.len()
    ));
    output
}

// --- Plan Mode Tools ---

/// Get the current agent mode
pub fn current_agent_mode() -> AgentMode {
    *AGENT_MODE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Check if plan mode is currently active
pub fn is_plan_mode() -> bool {
    current_agent_mode() == AgentMode::Plan
}

/// Set the agent mode
pub fn set_agent_mode(mode: AgentMode) {
    let mut m = AGENT_MODE.lock().unwrap_or_else(|e| e.into_inner());
    *m = mode;
}

async fn exec_plan_enter(arguments: &str) -> Result<String> {
    let args: PlanEnterArgs = serde_json::from_str(arguments)?;
    let mode = current_agent_mode();

    // Determine target mode: plan_name starting with "explore:" → explore mode
    let plan_name = args.plan_name.as_deref().unwrap_or("unnamed");
    let target_mode = if plan_name.starts_with("explore:") {
        AgentMode::Explore
    } else {
        AgentMode::Plan
    };

    if mode == target_mode {
        return Ok(format!("Already in {} mode.", mode));
    }

    set_agent_mode(target_mode);

    eprintln!(
        "  {} {} mode activated: {}",
        "!".yellow().bold(),
        target_mode,
        plan_name.cyan()
    );

    match target_mode {
        AgentMode::Explore => Ok(format!(
            "Explore mode activated ('{plan_name}'). \
             Only read/search tools are available. \
             Call plan_exit when ready to switch to build mode."
        )),
        AgentMode::Plan => Ok(format!(
            "Plan mode activated (plan: '{plan_name}'). \
             Write/edit/bash tools are now disabled. \
             Use read/search tools to explore the codebase and design your plan. \
             You can write plans to .bfcode/plans/. \
             Call plan_exit when ready to implement."
        )),
        _ => unreachable!(),
    }
}

async fn exec_plan_exit() -> Result<String> {
    let mode = current_agent_mode();
    if mode == AgentMode::Build {
        return Ok("Already in build mode.".to_string());
    }

    let prev = mode;
    set_agent_mode(AgentMode::Build);

    eprintln!(
        "  {} Build mode activated (was: {prev}) — all tools now available",
        "!".green().bold()
    );

    Ok(format!(
        "{prev} mode deactivated. Build mode active — all tools are now available. \
         Proceed with implementation."
    ))
}

/// Simple shell escape for arguments
pub(crate) fn shell_escape(s: &str) -> String {
    if s.contains('\'') {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else {
        format!("'{s}'")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- truncate_output ---

    #[test]
    fn test_truncate_output_short() {
        let input = "hello world";
        assert_eq!(truncate_output(input), input);
    }

    #[test]
    fn test_truncate_output_many_lines() {
        let lines: String = (0..2500).map(|i| format!("line {i}\n")).collect();
        let result = truncate_output(&lines);
        assert!(result.contains("Output truncated"));
        assert!(result.contains("2000 lines shown"));
    }

    #[test]
    fn test_truncate_output_large_bytes() {
        // Create output under line limit but over byte limit
        let line = "x".repeat(1000);
        let lines: String = (0..100).map(|_| format!("{line}\n")).collect();
        assert!(lines.len() > MAX_OUTPUT_BYTES);
        let result = truncate_output(&lines);
        assert!(result.contains("Output truncated"));
        assert!(result.contains("bytes shown"));
    }

    #[test]
    fn test_truncate_output_empty() {
        assert_eq!(truncate_output(""), "");
    }

    // --- shell_escape ---

    #[test]
    fn test_shell_escape_simple() {
        assert_eq!(shell_escape("hello"), "'hello'");
    }

    #[test]
    fn test_shell_escape_with_single_quote() {
        assert_eq!(shell_escape("it's"), "\"it's\"");
    }

    #[test]
    fn test_shell_escape_with_spaces() {
        assert_eq!(shell_escape("hello world"), "'hello world'");
    }

    #[test]
    fn test_shell_escape_with_double_quote() {
        // No single quotes, so wrapped in single quotes
        assert_eq!(shell_escape(r#"say "hi""#), r#"'say "hi"'"#);
    }

    #[test]
    fn test_shell_escape_with_both_quotes() {
        // Has single quote, so wrapped in double quotes with escaped double quotes
        let result = shell_escape(r#"it's "cool""#);
        assert!(result.starts_with('"'));
        assert!(result.contains(r#"\""#));
    }

    // --- get_tool_definitions ---

    #[test]
    fn test_tool_definitions_count() {
        let defs = get_tool_definitions();
        // Base: 28 tools, +1 if BRAVE/TAVILY key set, +1 if OPENAI key set
        assert!(
            defs.len() >= 28 && defs.len() <= 30,
            "got {} tools",
            defs.len()
        );
    }

    #[test]
    fn test_tool_definitions_names() {
        let defs = get_tool_definitions();
        let names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();
        assert!(names.contains(&"read"));
        assert!(names.contains(&"write"));
        assert!(names.contains(&"edit"));
        assert!(names.contains(&"bash"));
        assert!(names.contains(&"glob"));
        assert!(names.contains(&"grep"));
        assert!(names.contains(&"list_files"));
    }

    #[test]
    fn test_tool_definitions_all_have_descriptions() {
        let defs = get_tool_definitions();
        for def in &defs {
            assert!(
                !def.function.description.is_empty(),
                "Tool {} has empty description",
                def.function.name
            );
            assert_eq!(def.tool_type, "function");
        }
    }

    #[test]
    fn test_tool_definitions_parameters_are_objects() {
        let defs = get_tool_definitions();
        for def in &defs {
            let params = &def.function.parameters;
            assert_eq!(
                params["type"], "object",
                "Tool {} params not an object",
                def.function.name
            );
            assert!(
                params["properties"].is_object(),
                "Tool {} has no properties",
                def.function.name
            );
        }
    }

    // --- Permissions ---

    #[test]
    fn test_permissions_new_denies_by_default() {
        let perms = Permissions::new();
        assert!(!perms.is_allowed("bash:ls"));
    }

    #[test]
    fn test_permissions_allow_always() {
        let perms = Permissions::new();
        perms.allow_always("bash:*");
        assert!(perms.is_allowed("bash:ls"));
        assert!(perms.is_allowed("bash:cargo build"));
    }

    #[test]
    fn test_permissions_exact_match() {
        let perms = Permissions::new();
        perms.allow_always("bash:ls");
        assert!(perms.is_allowed("bash:ls"));
        assert!(!perms.is_allowed("bash:rm"));
    }

    #[test]
    fn test_permissions_wildcard_doesnt_cross_tools() {
        let perms = Permissions::new();
        perms.allow_always("bash:*");
        assert!(!perms.is_allowed("write:foo.txt"));
    }

    // --- Tool execution (integration-style) ---

    #[tokio::test]
    async fn test_exec_read_file() {
        // Write a temp file, then read it
        let dir = std::env::temp_dir().join("bfcode_test_read");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("test.txt");
        std::fs::write(&file, "line1\nline2\nline3\n").unwrap();

        let args = format!(r#"{{"path": "{}"}}"#, file.display());
        let result = exec_read(&args).await.unwrap();
        assert!(result.contains("line1"));
        assert!(result.contains("line2"));
        assert!(result.contains("line3"));
        // Check line numbers
        assert!(result.contains("1\t"));
        assert!(result.contains("2\t"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_exec_read_with_offset_limit() {
        let dir = std::env::temp_dir().join("bfcode_test_read_offset");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("test.txt");
        let content: String = (1..=20).map(|i| format!("line{i}\n")).collect();
        std::fs::write(&file, &content).unwrap();

        let args = format!(
            r#"{{"path": "{}", "offset": 5, "limit": 3}}"#,
            file.display()
        );
        let result = exec_read(&args).await.unwrap();
        assert!(result.contains("line5"));
        assert!(result.contains("line6"));
        assert!(result.contains("line7"));
        assert!(!result.contains("line4"));
        assert!(!result.contains("line8"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_exec_read_nonexistent() {
        let args = r#"{"path": "/tmp/bfcode_nonexistent_file_xyz.txt"}"#;
        let result = exec_read(args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_exec_write_and_read_back() {
        let dir = std::env::temp_dir().join("bfcode_test_write");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("output.txt");

        let args = format!(
            r#"{{"path": "{}", "content": "hello\nworld"}}"#,
            file.display()
        );
        let result = exec_write(&args, "test").await.unwrap();
        assert!(result.contains("Wrote"));
        assert!(result.contains("2 lines"));

        let written = std::fs::read_to_string(&file).unwrap();
        assert_eq!(written, "hello\nworld");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_exec_write_creates_parent_dirs() {
        let dir = std::env::temp_dir().join("bfcode_test_write_nested");
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("a").join("b").join("c.txt");

        let args = format!(r#"{{"path": "{}", "content": "deep"}}"#, file.display());
        let result = exec_write(&args, "test").await.unwrap();
        assert!(result.contains("Wrote"));
        assert!(file.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_exec_edit_single_replace() {
        let dir = std::env::temp_dir().join("bfcode_test_edit");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("edit.txt");
        std::fs::write(&file, "foo bar baz").unwrap();

        let args = format!(
            r#"{{"path": "{}", "old_string": "bar", "new_string": "qux"}}"#,
            file.display()
        );
        let result = exec_edit(&args, "test").await.unwrap();
        assert!(result.contains("Edited"));
        assert!(result.contains("1 occurrence"));

        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "foo qux baz");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_exec_edit_replace_all() {
        let dir = std::env::temp_dir().join("bfcode_test_edit_all");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("edit.txt");
        std::fs::write(&file, "aaa bbb aaa").unwrap();

        let args = format!(
            r#"{{"path": "{}", "old_string": "aaa", "new_string": "ccc", "replace_all": true}}"#,
            file.display()
        );
        let result = exec_edit(&args, "test").await.unwrap();
        assert!(result.contains("2 occurrence"));

        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "ccc bbb ccc");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_exec_edit_no_match() {
        let dir = std::env::temp_dir().join("bfcode_test_edit_nomatch");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("edit.txt");
        std::fs::write(&file, "hello world").unwrap();

        let args = format!(
            r#"{{"path": "{}", "old_string": "xyz", "new_string": "abc"}}"#,
            file.display()
        );
        let result = exec_edit(&args, "test").await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_exec_edit_same_string_error() {
        let dir = std::env::temp_dir().join("bfcode_test_edit_same");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("edit.txt");
        std::fs::write(&file, "hello").unwrap();

        let args = format!(
            r#"{{"path": "{}", "old_string": "hello", "new_string": "hello"}}"#,
            file.display()
        );
        let result = exec_edit(&args, "test").await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("must be different")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_exec_edit_multiple_matches_no_replace_all() {
        let dir = std::env::temp_dir().join("bfcode_test_edit_multi");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("edit.txt");
        std::fs::write(&file, "aaa bbb aaa").unwrap();

        let args = format!(
            r#"{{"path": "{}", "old_string": "aaa", "new_string": "ccc"}}"#,
            file.display()
        );
        let result = exec_edit(&args, "test").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("2 matches"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_exec_bash_echo() {
        let args = r#"{"command": "echo hello"}"#;
        let result = exec_bash(args).await.unwrap();
        assert!(result.contains("exit code: 0"));
        assert!(result.contains("hello"));
    }

    #[tokio::test]
    async fn test_exec_bash_nonzero_exit() {
        let args = r#"{"command": "exit 42"}"#;
        let result = exec_bash(args).await.unwrap();
        assert!(result.contains("exit code: 42"));
    }

    #[tokio::test]
    async fn test_exec_bash_stderr() {
        let args = r#"{"command": "echo err >&2"}"#;
        let result = exec_bash(args).await.unwrap();
        assert!(result.contains("stderr:"));
        assert!(result.contains("err"));
    }

    #[tokio::test]
    async fn test_exec_bash_timeout() {
        let args = r#"{"command": "sleep 10", "timeout": 1}"#;
        let result = exec_bash(args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn test_exec_list_files() {
        let dir = std::env::temp_dir().join("bfcode_test_list");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("subdir")).unwrap();
        std::fs::write(dir.join("a.txt"), "hello").unwrap();
        std::fs::write(dir.join("b.rs"), "world").unwrap();

        let args = format!(r#"{{"path": "{}"}}"#, dir.display());
        let result = exec_list_files(&args).await.unwrap();
        assert!(result.contains("a.txt"));
        assert!(result.contains("b.rs"));
        assert!(result.contains("subdir/"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_exec_grep_finds_pattern() {
        let dir = std::env::temp_dir().join("bfcode_test_grep");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "hello world\nfoo bar\n").unwrap();
        std::fs::write(dir.join("b.txt"), "no match here\n").unwrap();

        let args = format!(r#"{{"pattern": "hello", "path": "{}"}}"#, dir.display());
        let result = exec_grep(&args).await.unwrap();
        assert!(result.contains("hello world"));
        assert!(result.contains("a.txt"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_exec_grep_no_match() {
        let dir = std::env::temp_dir().join("bfcode_test_grep_none");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "hello\n").unwrap();

        let args = format!(r#"{{"pattern": "zzzzz", "path": "{}"}}"#, dir.display());
        let result = exec_grep(&args).await.unwrap();
        assert!(result.contains("No matches found"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- execute_tool unknown tool ---

    #[tokio::test]
    async fn test_execute_unknown_tool() {
        let perms = Permissions::new();
        let result = execute_tool("unknown_tool", "{}", &perms, "test").await;
        assert!(result.contains("Error"));
        assert!(result.contains("Unknown tool"));
    }

    // --- Unified diff parsing ---

    #[test]
    fn test_parse_unified_diff_single_file() {
        let patch = "\
--- a/foo.txt
+++ b/foo.txt
@@ -1,3 +1,3 @@
 line1
-old line
+new line
 line3
";
        let patches = parse_unified_diff(patch).unwrap();
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].target_path, "foo.txt");
        assert_eq!(patches[0].hunks.len(), 1);
    }

    #[test]
    fn test_parse_unified_diff_multi_file() {
        let patch = "\
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
--- a/b.txt
+++ b/b.txt
@@ -1 +1 @@
-foo
+bar
";
        let patches = parse_unified_diff(patch).unwrap();
        assert_eq!(patches.len(), 2);
        assert_eq!(patches[0].target_path, "a.txt");
        assert_eq!(patches[1].target_path, "b.txt");
    }

    #[test]
    fn test_parse_hunk_header() {
        let (os, oc, ns, nc) = parse_hunk_header("@@ -1,5 +1,7 @@ fn main").unwrap();
        assert_eq!((os, oc, ns, nc), (1, 5, 1, 7));
    }

    #[test]
    fn test_parse_hunk_header_single_line() {
        let (os, oc, ns, nc) = parse_hunk_header("@@ -1 +1 @@").unwrap();
        assert_eq!((os, oc, ns, nc), (1, 1, 1, 1));
    }

    #[test]
    fn test_apply_hunks_simple_replace() {
        let content = "line1\nold line\nline3\n";
        let hunks = vec![Hunk {
            old_start: 1,
            _old_count: 3,
            _new_start: 1,
            _new_count: 3,
            lines: vec![
                DiffLine::Context("line1".into()),
                DiffLine::Remove("old line".into()),
                DiffLine::Add("new line".into()),
                DiffLine::Context("line3".into()),
            ],
        }];
        let result = apply_hunks(content, &hunks).unwrap();
        assert!(result.contains("new line"));
        assert!(!result.contains("old line"));
        assert!(result.contains("line1"));
        assert!(result.contains("line3"));
    }

    #[test]
    fn test_apply_hunks_add_lines() {
        let content = "line1\nline2\n";
        let hunks = vec![Hunk {
            old_start: 1,
            _old_count: 2,
            _new_start: 1,
            _new_count: 3,
            lines: vec![
                DiffLine::Context("line1".into()),
                DiffLine::Add("inserted".into()),
                DiffLine::Context("line2".into()),
            ],
        }];
        let result = apply_hunks(content, &hunks).unwrap();
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines, vec!["line1", "inserted", "line2"]);
    }

    #[test]
    fn test_apply_hunks_remove_lines() {
        let content = "line1\ndelete_me\nline3\n";
        let hunks = vec![Hunk {
            old_start: 1,
            _old_count: 3,
            _new_start: 1,
            _new_count: 2,
            lines: vec![
                DiffLine::Context("line1".into()),
                DiffLine::Remove("delete_me".into()),
                DiffLine::Context("line3".into()),
            ],
        }];
        let result = apply_hunks(content, &hunks).unwrap();
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines, vec!["line1", "line3"]);
    }

    #[test]
    fn test_apply_hunks_new_file() {
        let content = "";
        let hunks = vec![Hunk {
            old_start: 0,
            _old_count: 0,
            _new_start: 1,
            _new_count: 2,
            lines: vec![
                DiffLine::Add("new line 1".into()),
                DiffLine::Add("new line 2".into()),
            ],
        }];
        let result = apply_hunks(content, &hunks).unwrap();
        assert!(result.contains("new line 1"));
        assert!(result.contains("new line 2"));
    }

    #[tokio::test]
    async fn test_exec_apply_patch_creates_file() {
        let tmp = std::env::temp_dir().join(format!("bfcode_patch_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let target = tmp.join("new_file.txt");
        let patch = format!(
            "--- /dev/null\n+++ b/{}\n@@ -0,0 +1,2 @@\n+hello\n+world\n",
            target.display()
        );
        let args = serde_json::json!({"patch": patch}).to_string();

        let result = exec_apply_patch(&args, "test_session").await.unwrap();
        assert!(result.contains("A "));
        assert!(target.exists());
        let content = std::fs::read_to_string(&target).unwrap();
        assert!(content.contains("hello"));
        assert!(content.contains("world"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_tool_definitions_includes_apply_patch() {
        let defs = get_tool_definitions();
        assert!(defs.iter().any(|d| d.function.name == "apply_patch"));
    }

    // ── apply_patch: multi-hunk ──────────────────────────────────────

    #[test]
    fn test_apply_hunks_multiple_hunks() {
        let content = "aaa\nbbb\nccc\nddd\neee\nfff\nggg\n";
        let hunks = vec![
            Hunk {
                old_start: 1,
                _old_count: 3,
                _new_start: 1,
                _new_count: 3,
                lines: vec![
                    DiffLine::Context("aaa".into()),
                    DiffLine::Remove("bbb".into()),
                    DiffLine::Add("BBB".into()),
                    DiffLine::Context("ccc".into()),
                ],
            },
            Hunk {
                old_start: 5,
                _old_count: 3,
                _new_start: 5,
                _new_count: 3,
                lines: vec![
                    DiffLine::Context("eee".into()),
                    DiffLine::Remove("fff".into()),
                    DiffLine::Add("FFF".into()),
                    DiffLine::Context("ggg".into()),
                ],
            },
        ];
        let result = apply_hunks(content, &hunks).unwrap();
        assert!(result.contains("BBB"));
        assert!(result.contains("FFF"));
        assert!(!result.contains("\nbbb\n"));
        assert!(!result.contains("\nfff\n"));
    }

    // ── apply_patch: parse edge cases ────────────────────────────────

    #[test]
    fn test_parse_unified_diff_no_newline_marker() {
        let patch = "\
--- a/foo.txt
+++ b/foo.txt
@@ -1,2 +1,2 @@
-old
+new
\\ No newline at end of file
";
        let patches = parse_unified_diff(patch).unwrap();
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].hunks.len(), 1);
        // The marker line should be skipped, not treated as diff content
        let hunk_lines = &patches[0].hunks[0].lines;
        assert_eq!(hunk_lines.len(), 2); // just Remove + Add
    }

    #[test]
    fn test_parse_unified_diff_empty_patch() {
        let patches = parse_unified_diff("").unwrap();
        assert!(patches.is_empty());
    }

    #[test]
    fn test_parse_unified_diff_garbage_input() {
        let patches = parse_unified_diff("this is not a patch\nrandom text\n").unwrap();
        assert!(patches.is_empty());
    }

    #[test]
    fn test_parse_unified_diff_with_context_line() {
        let patch = "\
--- a/foo.txt
+++ b/foo.txt
@@ -1,5 +1,5 @@ function context
 line1
 line2
-old
+new
 line4
 line5
";
        let patches = parse_unified_diff(patch).unwrap();
        let hunk = &patches[0].hunks[0];
        assert_eq!(hunk.lines.len(), 6); // 2 context + remove + add + 2 context
    }

    // ── apply_patch: exec with existing file ─────────────────────────

    #[tokio::test]
    async fn test_exec_apply_patch_modifies_existing() {
        let tmp = std::env::temp_dir().join(format!("bfcode_patch_mod_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let target = tmp.join("existing.txt");
        std::fs::write(&target, "line1\nold_line\nline3\n").unwrap();

        let patch = format!(
            "--- a/{path}\n+++ b/{path}\n@@ -1,3 +1,3 @@\n line1\n-old_line\n+new_line\n line3\n",
            path = target.display()
        );
        let args = serde_json::json!({"patch": patch}).to_string();
        let result = exec_apply_patch(&args, "test").await.unwrap();
        assert!(result.contains("M "));

        let content = std::fs::read_to_string(&target).unwrap();
        assert!(content.contains("new_line"));
        assert!(!content.contains("old_line"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── apply_patch: empty patch error ───────────────────────────────

    #[tokio::test]
    async fn test_exec_apply_patch_empty_patch_error() {
        let args = serde_json::json!({"patch": ""}).to_string();
        let result = exec_apply_patch(&args, "test").await;
        assert!(result.is_err());
    }

    // ── apply_patch: parse range ─────────────────────────────────────

    #[test]
    fn test_parse_range_with_comma() {
        let (start, count) = parse_range("10,5").unwrap();
        assert_eq!(start, 10);
        assert_eq!(count, 5);
    }

    #[test]
    fn test_parse_range_single_number() {
        let (start, count) = parse_range("42").unwrap();
        assert_eq!(start, 42);
        assert_eq!(count, 1);
    }

    // ── tool permission summary for apply_patch ──────────────────────

    #[test]
    fn test_tool_permission_summary_apply_patch() {
        let args = r#"{"patch": "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-x\n+y"}"#;
        let summary = tool_permission_summary("apply_patch", args);
        assert!(summary.contains("2 file(s)"));
    }

    // ── Protected files ─────────────────────────────────────────────

    #[test]
    fn test_is_protected_env() {
        assert!(is_protected_path(".env"));
        assert!(is_protected_path(".env.local"));
        assert!(is_protected_path(".env.production"));
        assert!(is_protected_path("config/.env"));
    }

    #[test]
    fn test_is_protected_keys() {
        assert!(is_protected_path("id_rsa"));
        assert!(is_protected_path("~/.ssh/id_rsa"));
        assert!(is_protected_path("server.pem"));
        assert!(is_protected_path("cert.key"));
        assert!(is_protected_path("keystore.p12"));
    }

    #[test]
    fn test_is_protected_credentials() {
        assert!(is_protected_path("credentials.json"));
        assert!(is_protected_path("secrets.json"));
        assert!(is_protected_path("service-account.json"));
    }

    #[test]
    fn test_not_protected_normal_files() {
        assert!(!is_protected_path("main.rs"));
        assert!(!is_protected_path("src/lib.rs"));
        assert!(!is_protected_path("config.json"));
        assert!(!is_protected_path("Cargo.toml"));
        assert!(!is_protected_path("README.md"));
    }

    #[test]
    fn test_check_protected_file_write() {
        let args = r#"{"path": ".env", "content": "SECRET=abc"}"#;
        let result = check_protected_file("write", args);
        assert!(result.is_some());
        assert!(result.unwrap().contains("protected"));
    }

    #[test]
    fn test_check_protected_file_edit() {
        let args = r#"{"path": ".env.local", "old_string": "a", "new_string": "b"}"#;
        let result = check_protected_file("edit", args);
        assert!(result.is_some());
    }

    #[test]
    fn test_check_protected_file_normal() {
        let args = r#"{"path": "src/main.rs", "content": "fn main() {}"}"#;
        let result = check_protected_file("write", args);
        assert!(result.is_none());
    }

    #[test]
    fn test_check_protected_file_patch() {
        let args = r#"{"patch": "--- a/.env\n+++ b/.env\n@@ -1 +1 @@\n-old\n+new"}"#;
        let result = check_protected_file("apply_patch", args);
        assert!(result.is_some());
    }

    #[test]
    fn test_check_protected_file_patch_normal() {
        let args = r#"{"patch": "--- a/main.rs\n+++ b/main.rs\n@@ -1 +1 @@\n-old\n+new"}"#;
        let result = check_protected_file("apply_patch", args);
        assert!(result.is_none());
    }

    // ── HTML stripping ──────────────────────────────────────────────

    #[test]
    fn test_strip_html_tags_simple() {
        assert_eq!(strip_html_tags("<p>hello</p>"), "hello");
    }

    #[test]
    fn test_strip_html_tags_nested() {
        assert_eq!(
            strip_html_tags("<div><p>hello <b>world</b></p></div>"),
            "hello world"
        );
    }

    #[test]
    fn test_strip_html_tags_entities() {
        assert_eq!(strip_html_tags("a &amp; b &lt; c"), "a & b < c");
    }

    #[test]
    fn test_strip_html_tags_script_removed() {
        let html = "<p>hello</p><script>alert('xss')</script><p>world</p>";
        let result = strip_html_tags(html);
        assert!(result.contains("hello"));
        assert!(result.contains("world"));
        assert!(!result.contains("alert"));
    }

    #[test]
    fn test_strip_html_tags_style_removed() {
        let html = "<style>.foo { color: red; }</style><p>content</p>";
        let result = strip_html_tags(html);
        assert!(result.contains("content"));
        assert!(!result.contains("color"));
    }

    #[test]
    fn test_strip_html_tags_plain_text() {
        assert_eq!(strip_html_tags("just plain text"), "just plain text");
    }

    #[test]
    fn test_strip_html_collapses_whitespace() {
        let result = strip_html_tags("<p>hello</p>  <p>world</p>");
        // Should not have excessive whitespace
        assert!(!result.contains("  "));
    }

    // ── Tool definitions include new tools ───────────────────────────

    #[test]
    fn test_tool_definitions_includes_webfetch() {
        let defs = get_tool_definitions();
        assert!(defs.iter().any(|d| d.function.name == "webfetch"));
    }

    #[test]
    fn test_tool_definitions_includes_memory_tools() {
        let defs = get_tool_definitions();
        assert!(defs.iter().any(|d| d.function.name == "memory_save"));
        assert!(defs.iter().any(|d| d.function.name == "memory_delete"));
        assert!(defs.iter().any(|d| d.function.name == "memory_list"));
    }

    // Lock for tests that modify global agent mode (prevents races between parallel tests)
    static AGENT_MODE_LOCK: Mutex<()> = Mutex::new(());

    // --- Oneshot mode auto-approve ---

    fn permissions_with_auto_approve() -> Permissions {
        Permissions {
            always_allowed: Mutex::new(HashSet::new()),
            auto_approve: AtomicBool::new(true),
        }
    }

    #[test]
    fn test_permissions_oneshot_auto_approve() {
        let perms = permissions_with_auto_approve();
        // ask_permission should auto-approve without reading stdin
        let reply = perms.ask_permission("bash", "echo hello");
        assert!(matches!(reply, PermissionReply::Allow));

        let reply = perms.ask_permission("write", "foo.txt (10 bytes)");
        assert!(matches!(reply, PermissionReply::Allow));

        let reply = perms.ask_permission("edit", "bar.rs (1 occurrence)");
        assert!(matches!(reply, PermissionReply::Allow));
    }

    #[test]
    fn test_permissions_no_auto_approve_not_pre_allowed() {
        let perms = Permissions {
            always_allowed: Mutex::new(HashSet::new()),
            auto_approve: AtomicBool::new(false),
        };
        // Without auto_approve and without allow_always, the tool should NOT be pre-allowed
        assert!(!perms.is_allowed("bash:echo hello"));
    }

    #[tokio::test]
    async fn test_execute_tool_oneshot_allows_bash() {
        let perms = permissions_with_auto_approve();
        let result = execute_tool(
            "bash",
            r#"{"command": "echo oneshot_test"}"#,
            &perms,
            "test",
        )
        .await;
        assert!(result.contains("oneshot_test"));
        assert!(result.contains("exit code: 0"));
    }

    #[tokio::test]
    async fn test_execute_tool_oneshot_allows_write() {
        let _lock = AGENT_MODE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_agent_mode(AgentMode::Build);
        let dir = std::env::temp_dir().join("bfcode_test_oneshot_write");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("oneshot.txt");

        let perms = permissions_with_auto_approve();
        let args = format!(
            r#"{{"path": "{}", "content": "written in oneshot mode"}}"#,
            file.display()
        );
        let result = execute_tool("write", &args, &perms, "test").await;
        assert!(result.contains("Wrote"));

        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "written in oneshot mode");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ==============================
    // Multi-Edit Tool Tests
    // ==============================

    #[test]
    fn test_tool_definitions_include_new_tools() {
        let defs = get_tool_definitions();
        let names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();
        assert!(names.contains(&"multiedit"), "missing multiedit");
        assert!(names.contains(&"batch"), "missing batch");
        assert!(names.contains(&"task"), "missing task");
        assert!(names.contains(&"todowrite"), "missing todowrite");
        assert!(names.contains(&"todoread"), "missing todoread");
        assert!(names.contains(&"plan_enter"), "missing plan_enter");
        assert!(names.contains(&"plan_exit"), "missing plan_exit");
    }

    #[tokio::test]
    async fn test_multiedit_basic() {
        let dir = crate::test_utils::tmp_dir("multiedit_basic");
        let file = dir.join("test.txt");
        std::fs::write(&file, "hello world\nfoo bar\nbaz qux\n").unwrap();

        let args = serde_json::json!({
            "path": file.display().to_string(),
            "edits": [
                {"old_string": "hello", "new_string": "HELLO"},
                {"old_string": "foo", "new_string": "FOO"}
            ]
        });
        let result = exec_multiedit(&args.to_string(), "test_session")
            .await
            .unwrap();
        assert!(result.contains("applied 2 edits"));

        let content = std::fs::read_to_string(&file).unwrap();
        assert!(content.contains("HELLO world"));
        assert!(content.contains("FOO bar"));
        assert!(content.contains("baz qux"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_multiedit_sequential_dependency() {
        // Second edit depends on first edit's result
        let dir = crate::test_utils::tmp_dir("multiedit_seq");
        let file = dir.join("test.txt");
        std::fs::write(&file, "aaa bbb ccc").unwrap();

        let args = serde_json::json!({
            "path": file.display().to_string(),
            "edits": [
                {"old_string": "aaa", "new_string": "xxx"},
                {"old_string": "xxx bbb", "new_string": "REPLACED"}
            ]
        });
        let result = exec_multiedit(&args.to_string(), "test_session")
            .await
            .unwrap();
        assert!(result.contains("applied 2 edits"));

        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "REPLACED ccc");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_multiedit_empty_edits() {
        let result = exec_multiedit(r#"{"path": "/tmp/x", "edits": []}"#, "test").await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("must not be empty")
        );
    }

    #[tokio::test]
    async fn test_multiedit_not_found() {
        let dir = crate::test_utils::tmp_dir("multiedit_nf");
        let file = dir.join("test.txt");
        std::fs::write(&file, "hello world").unwrap();

        let args = serde_json::json!({
            "path": file.display().to_string(),
            "edits": [
                {"old_string": "missing", "new_string": "replaced"}
            ]
        });
        let result = exec_multiedit(&args.to_string(), "test").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_multiedit_replace_all() {
        let dir = crate::test_utils::tmp_dir("multiedit_ra");
        let file = dir.join("test.txt");
        std::fs::write(&file, "aaa bbb aaa ccc aaa").unwrap();

        let args = serde_json::json!({
            "path": file.display().to_string(),
            "edits": [
                {"old_string": "aaa", "new_string": "XXX", "replace_all": true}
            ]
        });
        let result = exec_multiedit(&args.to_string(), "test").await.unwrap();
        assert!(result.contains("3 total replacements"));

        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "XXX bbb XXX ccc XXX");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_multiedit_same_old_new() {
        let dir = crate::test_utils::tmp_dir("multiedit_same");
        let file = dir.join("test.txt");
        std::fs::write(&file, "hello").unwrap();

        let args = serde_json::json!({
            "path": file.display().to_string(),
            "edits": [
                {"old_string": "hello", "new_string": "hello"}
            ]
        });
        let result = exec_multiedit(&args.to_string(), "test").await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("must be different")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ==============================
    // Batch Tool Tests
    // ==============================

    #[tokio::test]
    async fn test_batch_parallel_reads() {
        let dir = crate::test_utils::tmp_dir("batch_reads");
        let f1 = dir.join("a.txt");
        let f2 = dir.join("b.txt");
        std::fs::write(&f1, "file A content").unwrap();
        std::fs::write(&f2, "file B content").unwrap();

        let perms = permissions_with_auto_approve();
        let args = serde_json::json!({
            "tool_calls": [
                {"tool": "read", "parameters": {"path": f1.display().to_string()}},
                {"tool": "read", "parameters": {"path": f2.display().to_string()}}
            ]
        });
        let result = exec_batch(&args.to_string(), &perms, "test").await.unwrap();
        assert!(result.contains("file A content"));
        assert!(result.contains("file B content"));
        assert!(result.contains("Batch executed 2 tool calls"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_batch_empty_calls() {
        let perms = permissions_with_auto_approve();
        let result = exec_batch(r#"{"tool_calls": []}"#, &perms, "test").await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("must not be empty")
        );
    }

    #[tokio::test]
    async fn test_batch_too_many_calls() {
        let perms = permissions_with_auto_approve();
        let calls: Vec<serde_json::Value> = (0..26)
            .map(|i| serde_json::json!({"tool": "read", "parameters": {"path": format!("/tmp/f{i}")}}))
            .collect();
        let args = serde_json::json!({"tool_calls": calls});
        let result = exec_batch(&args.to_string(), &perms, "test").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Maximum is 25"));
    }

    #[tokio::test]
    async fn test_batch_disallowed_nested() {
        let perms = permissions_with_auto_approve();
        let args = serde_json::json!({
            "tool_calls": [
                {"tool": "batch", "parameters": {"tool_calls": []}}
            ]
        });
        let result = exec_batch(&args.to_string(), &perms, "test").await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("cannot be used inside batch")
        );
    }

    #[tokio::test]
    async fn test_batch_disallowed_task() {
        let perms = permissions_with_auto_approve();
        let args = serde_json::json!({
            "tool_calls": [
                {"tool": "task", "parameters": {"description": "test", "prompt": "test"}}
            ]
        });
        let result = exec_batch(&args.to_string(), &perms, "test").await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("cannot be used inside batch")
        );
    }

    #[tokio::test]
    async fn test_batch_mixed_success_failure() {
        let dir = crate::test_utils::tmp_dir("batch_mixed");
        let f1 = dir.join("exists.txt");
        std::fs::write(&f1, "content").unwrap();

        let perms = permissions_with_auto_approve();
        let args = serde_json::json!({
            "tool_calls": [
                {"tool": "read", "parameters": {"path": f1.display().to_string()}},
                {"tool": "read", "parameters": {"path": "/nonexistent/file.txt"}}
            ]
        });
        let result = exec_batch(&args.to_string(), &perms, "test").await.unwrap();
        assert!(result.contains("content"));
        assert!(result.contains("Error"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ==============================
    // Todo Tool Tests
    // ==============================

    #[tokio::test]
    async fn test_todowrite_and_read() {
        let session_id = format!("test_todo_{}", std::process::id());
        let args = serde_json::json!({
            "todos": [
                {"content": "Implement feature A", "status": "pending", "priority": "high"},
                {"content": "Write tests", "status": "in_progress", "priority": "medium"},
                {"content": "Deploy", "status": "completed", "priority": "low"}
            ]
        });

        let result = exec_todowrite(&args.to_string(), &session_id)
            .await
            .unwrap();
        assert!(result.contains("3 items"));

        let read_result = exec_todoread(&session_id).await.unwrap();
        assert!(read_result.contains("Implement feature A"));
        assert!(read_result.contains("[HIGH]"));
        assert!(read_result.contains("Write tests"));
        assert!(read_result.contains("[~]")); // in_progress
        assert!(read_result.contains("Deploy"));
        assert!(read_result.contains("[x]")); // completed
        assert!(read_result.contains("1 pending"));
        assert!(read_result.contains("1 in progress"));
        assert!(read_result.contains("1 completed"));

        // Clean up
        let _ = std::fs::remove_file(format!(".bfcode/sessions/{session_id}_todos.json"));
    }

    #[tokio::test]
    async fn test_todoread_empty() {
        let session_id = format!("test_todo_empty_{}", std::process::id());
        let result = exec_todoread(&session_id).await.unwrap();
        assert!(result.contains("No todos"));
    }

    #[tokio::test]
    async fn test_todowrite_replace() {
        let session_id = format!("test_todo_replace_{}", std::process::id());

        // Write initial todos
        let args1 = serde_json::json!({
            "todos": [
                {"content": "Task 1", "status": "pending"},
                {"content": "Task 2", "status": "pending"}
            ]
        });
        exec_todowrite(&args1.to_string(), &session_id)
            .await
            .unwrap();

        // Replace with new list
        let args2 = serde_json::json!({
            "todos": [
                {"content": "Task 1", "status": "completed"},
                {"content": "Task 2", "status": "in_progress"},
                {"content": "Task 3", "status": "pending"}
            ]
        });
        let result = exec_todowrite(&args2.to_string(), &session_id)
            .await
            .unwrap();
        assert!(result.contains("3 items"));

        let read_result = exec_todoread(&session_id).await.unwrap();
        assert!(read_result.contains("Task 3"));
        assert!(read_result.contains("1 pending"));
        assert!(read_result.contains("1 in progress"));
        assert!(read_result.contains("1 completed"));

        // Clean up
        let _ = std::fs::remove_file(format!(".bfcode/sessions/{session_id}_todos.json"));
    }

    #[test]
    fn test_format_todos() {
        let items = vec![
            TodoItem {
                content: "High priority task".into(),
                status: TodoStatus::Pending,
                priority: TodoPriority::High,
            },
            TodoItem {
                content: "Normal task".into(),
                status: TodoStatus::InProgress,
                priority: TodoPriority::Medium,
            },
            TodoItem {
                content: "Done task".into(),
                status: TodoStatus::Completed,
                priority: TodoPriority::Low,
            },
            TodoItem {
                content: "Cancelled task".into(),
                status: TodoStatus::Cancelled,
                priority: TodoPriority::Medium,
            },
        ];
        let output = format_todos(&items);
        assert!(output.contains("[ ] High priority task [HIGH]"));
        assert!(output.contains("[~] Normal task"));
        assert!(output.contains("[x] Done task [low]"));
        assert!(output.contains("[-] Cancelled task"));
        assert!(output.contains("1 pending"));
        assert!(output.contains("1 in progress"));
        assert!(output.contains("1 completed"));
    }

    // ==============================
    // Plan Mode Tests
    // ==============================

    #[tokio::test]
    async fn test_plan_enter_exit() {
        let _lock = AGENT_MODE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_agent_mode(AgentMode::Build);
        assert!(!is_plan_mode());

        // Enter plan mode
        let result = exec_plan_enter(r#"{"plan_name": "test-plan"}"#)
            .await
            .unwrap();
        assert!(result.contains("Plan mode activated"));
        assert!(result.contains("test-plan"));
        assert!(is_plan_mode());

        // Double enter
        let result = exec_plan_enter(r#"{}"#).await.unwrap();
        assert!(result.contains("Already in plan mode"));

        // Exit plan mode
        let result = exec_plan_exit().await.unwrap();
        assert!(result.contains("Build mode active"));
        assert!(!is_plan_mode());

        // Double exit
        let result = exec_plan_exit().await.unwrap();
        assert!(result.contains("Already in build mode"));
    }

    #[tokio::test]
    async fn test_plan_mode_blocks_write_tools() {
        let _lock = AGENT_MODE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_agent_mode(AgentMode::Build);
        let _ = exec_plan_enter(r#"{"plan_name": "blocking-test"}"#).await;
        assert!(is_plan_mode());

        let perms = permissions_with_auto_approve();

        // Write should be blocked
        let result = execute_tool(
            "write",
            r#"{"path": "/tmp/test.txt", "content": "blocked"}"#,
            &perms,
            "test",
        )
        .await;
        assert!(result.contains("disabled in plan mode"));

        // Edit should be blocked
        let result = execute_tool(
            "edit",
            r#"{"path": "/tmp/test.txt", "old_string": "a", "new_string": "b"}"#,
            &perms,
            "test",
        )
        .await;
        assert!(result.contains("disabled in plan mode"));

        // Bash should be blocked
        let result = execute_tool("bash", r#"{"command": "echo hello"}"#, &perms, "test").await;
        assert!(result.contains("disabled in plan mode"));

        // Read should still work
        let dir = crate::test_utils::tmp_dir("plan_read");
        let file = dir.join("readable.txt");
        std::fs::write(&file, "can read this").unwrap();
        let result = execute_tool(
            "read",
            &format!(r#"{{"path": "{}"}}"#, file.display()),
            &perms,
            "test",
        )
        .await;
        assert!(result.contains("can read this"));

        // Clean up: exit plan mode
        let _ = exec_plan_exit().await;
        assert!(!is_plan_mode());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_plan_mode_allows_plan_writes() {
        let _lock = AGENT_MODE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_agent_mode(AgentMode::Build);
        let _ = exec_plan_enter(r#"{"plan_name": "write-test"}"#).await;

        let dir = crate::test_utils::tmp_dir("plan_write");
        let plan_dir = dir.join(".bfcode/plans");
        std::fs::create_dir_all(&plan_dir).unwrap();
        let plan_file = plan_dir.join("my-plan.md");

        let perms = permissions_with_auto_approve();
        let args = serde_json::json!({
            "path": plan_file.display().to_string(),
            "content": "# My Plan\n\nStep 1..."
        })
        .to_string();
        let result = execute_tool("write", &args, &perms, "test").await;
        assert!(
            result.contains("Wrote"),
            "Expected write to succeed in plan dir, got: {result}"
        );

        // Clean up
        let _ = exec_plan_exit().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ==============================
    // Explore Mode Tests
    // ==============================

    #[tokio::test]
    async fn test_explore_mode_blocks_all_writes() {
        let _lock = AGENT_MODE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_agent_mode(AgentMode::Build);
        let _ = exec_plan_enter(r#"{"plan_name": "explore:research"}"#).await;
        assert_eq!(current_agent_mode(), AgentMode::Explore);

        let perms = permissions_with_auto_approve();

        // Write blocked
        let result = execute_tool("write", r#"{"path":"/tmp/x","content":"y"}"#, &perms, "t").await;
        assert!(result.contains("disabled in explore mode"));

        // Bash blocked
        let result = execute_tool("bash", r#"{"command":"echo hi"}"#, &perms, "t").await;
        assert!(result.contains("disabled in explore mode"));

        // Read allowed
        let dir = crate::test_utils::tmp_dir("explore_read");
        let file = dir.join("r.txt");
        std::fs::write(&file, "readable").unwrap();
        let result = execute_tool(
            "read",
            &format!(r#"{{"path":"{}"}}"#, file.display()),
            &perms,
            "t",
        )
        .await;
        assert!(result.contains("readable"));

        // Grep allowed (tool exists)
        let result = execute_tool(
            "grep",
            r#"{"pattern":"hello","path":"/tmp/nonexistent_dir_xyz"}"#,
            &perms,
            "t",
        )
        .await;
        // Won't error from mode check — may error from no matches, but not "disabled"
        assert!(!result.contains("disabled in explore mode"));

        let _ = exec_plan_exit().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_agent_mode_transitions() {
        let _lock = AGENT_MODE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_agent_mode(AgentMode::Build);

        // Build → Plan
        let _ = exec_plan_enter(r#"{"plan_name": "test"}"#).await;
        assert_eq!(current_agent_mode(), AgentMode::Plan);

        // Plan → Build
        let _ = exec_plan_exit().await;
        assert_eq!(current_agent_mode(), AgentMode::Build);

        // Build → Explore
        let _ = exec_plan_enter(r#"{"plan_name": "explore:test"}"#).await;
        assert_eq!(current_agent_mode(), AgentMode::Explore);

        // Explore → Build
        let _ = exec_plan_exit().await;
        assert_eq!(current_agent_mode(), AgentMode::Build);
    }

    #[test]
    fn test_agent_mode_display() {
        assert_eq!(format!("{}", AgentMode::Build), "build");
        assert_eq!(format!("{}", AgentMode::Plan), "plan");
        assert_eq!(format!("{}", AgentMode::Explore), "explore");
    }

    // ==============================
    // Multiedit via execute_tool
    // ==============================

    #[tokio::test]
    async fn test_execute_tool_multiedit() {
        let dir = crate::test_utils::tmp_dir("exec_multiedit");
        let file = dir.join("test.txt");
        std::fs::write(&file, "alpha beta gamma").unwrap();

        let perms = permissions_with_auto_approve();
        let args = serde_json::json!({
            "path": file.display().to_string(),
            "edits": [
                {"old_string": "alpha", "new_string": "ALPHA"},
                {"old_string": "gamma", "new_string": "GAMMA"}
            ]
        });
        let result = execute_tool("multiedit", &args.to_string(), &perms, "test").await;
        assert!(result.contains("Multi-edited"));

        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "ALPHA beta GAMMA");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ==============================
    // Batch via execute_tool
    // ==============================

    #[tokio::test]
    async fn test_execute_tool_batch() {
        let dir = crate::test_utils::tmp_dir("exec_batch");
        let f1 = dir.join("x.txt");
        let f2 = dir.join("y.txt");
        std::fs::write(&f1, "xxx").unwrap();
        std::fs::write(&f2, "yyy").unwrap();

        let perms = permissions_with_auto_approve();
        let args = serde_json::json!({
            "tool_calls": [
                {"tool": "read", "parameters": {"path": f1.display().to_string()}},
                {"tool": "read", "parameters": {"path": f2.display().to_string()}}
            ]
        });
        let result = execute_tool("batch", &args.to_string(), &perms, "test").await;
        assert!(result.contains("xxx"));
        assert!(result.contains("yyy"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ==============================
    // Argument Parsing Tests
    // ==============================

    #[test]
    fn test_multiedit_args_parse() {
        let json = r#"{"path": "/tmp/test.txt", "edits": [{"old_string": "a", "new_string": "b"}, {"old_string": "c", "new_string": "d", "replace_all": true}]}"#;
        let args: MultiEditArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.path, "/tmp/test.txt");
        assert_eq!(args.edits.len(), 2);
        assert_eq!(args.edits[0].old_string, "a");
        assert_eq!(args.edits[1].replace_all, Some(true));
    }

    #[test]
    fn test_batch_args_parse() {
        let json = r#"{"tool_calls": [{"tool": "read", "parameters": {"path": "/tmp/x"}}, {"tool": "glob", "parameters": {"pattern": "*.rs"}}]}"#;
        let args: BatchArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.tool_calls.len(), 2);
        assert_eq!(args.tool_calls[0].tool, "read");
        assert_eq!(args.tool_calls[1].tool, "glob");
    }

    #[test]
    fn test_task_args_parse() {
        let json = r#"{"description": "explore codebase", "prompt": "Find all API endpoints", "subagent_type": "explore"}"#;
        let args: TaskToolArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.description, "explore codebase");
        assert_eq!(args.subagent_type, Some("explore".to_string()));
    }

    #[test]
    fn test_task_args_parse_minimal() {
        let json = r#"{"description": "test", "prompt": "do something"}"#;
        let args: TaskToolArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.subagent_type, None);
    }

    #[test]
    fn test_todowrite_args_parse() {
        let json = r#"{"todos": [{"content": "task 1", "status": "pending", "priority": "high"}, {"content": "task 2", "status": "in_progress"}]}"#;
        let args: TodoWriteArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.todos.len(), 2);
        assert_eq!(args.todos[0].priority, TodoPriority::High);
        assert_eq!(args.todos[1].priority, TodoPriority::Medium); // default
        assert_eq!(args.todos[1].status, TodoStatus::InProgress);
    }

    #[test]
    fn test_plan_enter_args_parse() {
        let json = r#"{"plan_name": "refactor-auth"}"#;
        let args: PlanEnterArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.plan_name, Some("refactor-auth".to_string()));

        let json = r#"{}"#;
        let args: PlanEnterArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.plan_name, None);
    }

    #[test]
    fn test_todo_status_serialization() {
        let json = serde_json::to_string(&TodoStatus::InProgress).unwrap();
        assert_eq!(json, r#""in_progress""#);
        let parsed: TodoStatus = serde_json::from_str(r#""completed""#).unwrap();
        assert_eq!(parsed, TodoStatus::Completed);
    }

    #[test]
    fn test_todo_priority_serialization() {
        let json = serde_json::to_string(&TodoPriority::High).unwrap();
        assert_eq!(json, r#""high""#);
        let parsed: TodoPriority = serde_json::from_str(r#""low""#).unwrap();
        assert_eq!(parsed, TodoPriority::Low);
    }
}
