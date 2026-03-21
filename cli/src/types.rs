use anyhow::Context;
use serde::{Deserialize, Serialize};

// --- Provider Types ---

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Grok,
    OpenAI,
    Anthropic,
}

impl Default for Provider {
    fn default() -> Self {
        Self::Grok
    }
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Provider::Grok => write!(f, "grok"),
            Provider::OpenAI => write!(f, "openai"),
            Provider::Anthropic => write!(f, "anthropic"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub provider: Provider,
    pub api_key_env: String,
    pub api_url: String,
    pub default_model: String,
    pub context_limit: u64,
}

/// Returns default provider configurations
pub fn provider_configs() -> Vec<ProviderConfig> {
    vec![
        ProviderConfig {
            provider: Provider::Grok,
            api_key_env: "GROK_API_KEY".into(),
            api_url: "https://api.x.ai/v1/chat/completions".into(),
            default_model: "grok-4-1-fast".into(),
            context_limit: 131_072,
        },
        ProviderConfig {
            provider: Provider::OpenAI,
            api_key_env: "OPENAI_API_KEY".into(),
            api_url: "https://api.openai.com/v1/chat/completions".into(),
            default_model: "gpt-4o".into(),
            context_limit: 128_000,
        },
        ProviderConfig {
            provider: Provider::Anthropic,
            api_key_env: "ANTHROPIC_API_KEY".into(),
            api_url: "https://api.anthropic.com/v1/messages".into(),
            default_model: "claude-sonnet-4-20250514".into(),
            context_limit: 200_000,
        },
    ]
}

/// Detect provider from model name
pub fn detect_provider(model: &str) -> Provider {
    if model.starts_with("claude") {
        Provider::Anthropic
    } else if model.starts_with("gpt-")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
    {
        Provider::OpenAI
    } else {
        Provider::Grok
    }
}

/// Get provider config for a given provider
pub fn get_provider_config(provider: &Provider) -> anyhow::Result<ProviderConfig> {
    provider_configs()
        .into_iter()
        .find(|c| c.provider == *provider)
        .context(format!("No provider config found for {}", provider))
}

/// Get context limit for a model
pub fn context_limit_for_model(model: &str) -> u64 {
    let provider = detect_provider(model);
    get_provider_config(&provider)
        .map(|c| c.context_limit)
        .unwrap_or(128_000)
}

/// Per-token cost in USD (per 1M tokens)
#[derive(Debug, Clone)]
pub struct ModelCost {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

/// Get cost info for a model. Prices per 1M tokens (USD).
pub fn model_cost(model: &str) -> ModelCost {
    match model {
        // Grok models
        m if m.starts_with("grok-4-1-fast") => ModelCost {
            input_per_million: 3.0,
            output_per_million: 15.0,
        },
        m if m.starts_with("grok-4-1") => ModelCost {
            input_per_million: 3.0,
            output_per_million: 15.0,
        },
        m if m.starts_with("grok-3") => ModelCost {
            input_per_million: 3.0,
            output_per_million: 15.0,
        },
        m if m.starts_with("grok") => ModelCost {
            input_per_million: 3.0,
            output_per_million: 15.0,
        },
        // OpenAI models
        m if m.starts_with("gpt-4o-mini") => ModelCost {
            input_per_million: 0.15,
            output_per_million: 0.60,
        },
        m if m.starts_with("gpt-4o") => ModelCost {
            input_per_million: 2.50,
            output_per_million: 10.0,
        },
        m if m.starts_with("gpt-4") => ModelCost {
            input_per_million: 30.0,
            output_per_million: 60.0,
        },
        m if m.starts_with("o4-mini") => ModelCost {
            input_per_million: 1.10,
            output_per_million: 4.40,
        },
        m if m.starts_with("o3-mini") => ModelCost {
            input_per_million: 1.10,
            output_per_million: 4.40,
        },
        m if m.starts_with("o3") => ModelCost {
            input_per_million: 10.0,
            output_per_million: 40.0,
        },
        m if m.starts_with("o1-mini") => ModelCost {
            input_per_million: 3.0,
            output_per_million: 12.0,
        },
        m if m.starts_with("o1") => ModelCost {
            input_per_million: 15.0,
            output_per_million: 60.0,
        },
        // Anthropic models
        m if m.contains("opus") => ModelCost {
            input_per_million: 15.0,
            output_per_million: 75.0,
        },
        m if m.contains("sonnet") => ModelCost {
            input_per_million: 3.0,
            output_per_million: 15.0,
        },
        m if m.contains("haiku") => ModelCost {
            input_per_million: 0.25,
            output_per_million: 1.25,
        },
        // Default fallback
        _ => ModelCost {
            input_per_million: 3.0,
            output_per_million: 15.0,
        },
    }
}

/// Calculate cost in USD from token counts
pub fn calculate_cost(model: &str, input_tokens: u64, output_tokens: u64) -> f64 {
    let cost = model_cost(model);
    (input_tokens as f64 * cost.input_per_million / 1_000_000.0)
        + (output_tokens as f64 * cost.output_per_million / 1_000_000.0)
}

/// Format cost as a readable string
pub fn format_cost(cost: f64) -> String {
    if cost < 0.01 {
        format!("${:.4}", cost)
    } else {
        format!("${:.2}", cost)
    }
}

/// Protected file patterns — prevent accidental writes to sensitive files
pub const PROTECTED_FILE_PATTERNS: &[&str] = &[
    ".env",
    ".env.local",
    ".env.production",
    ".env.development",
    ".env.staging",
    ".env.test",
    "credentials.json",
    "secrets.json",
    "secret.json",
    "service-account.json",
    ".npmrc",
    ".pypirc",
    "id_rsa",
    "id_ed25519",
    "id_ecdsa",
    ".pem",
    ".key",
    ".p12",
    ".pfx",
    ".keystore",
];

// --- Chat Completion Request (OpenAI-compatible) ---

#[derive(Serialize, Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub stream: bool,
    pub temperature: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
}

/// An image attachment embedded in a message
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ImageAttachment {
    /// Base64-encoded image data
    pub data: String,
    /// MIME type (e.g., "image/png", "image/jpeg")
    pub media_type: String,
}

// Single Message struct with optional fields to handle all OpenAI message shapes
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Message {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Image attachments (stored for context, converted at API call time)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageAttachment>>,
}

impl Message {
    /// Check if this message has image attachments
    pub fn has_images(&self) -> bool {
        self.images.as_ref().map(|v| !v.is_empty()).unwrap_or(false)
    }

    /// Convert to OpenAI JSON format (handles image content arrays)
    pub fn to_openai_json(&self) -> serde_json::Value {
        if self.has_images() && self.role == "user" {
            let images = self.images.as_deref().unwrap_or_default();
            let mut content_parts: Vec<serde_json::Value> = Vec::new();
            if let Some(text) = &self.content {
                content_parts.push(serde_json::json!({"type": "text", "text": text}));
            }
            for img in images {
                content_parts.push(serde_json::json!({
                    "type": "image_url",
                    "image_url": {
                        "url": format!("data:{};base64,{}", img.media_type, img.data)
                    }
                }));
            }
            let mut obj = serde_json::json!({"role": self.role, "content": content_parts});
            if let Some(tc) = &self.tool_calls {
                obj["tool_calls"] = serde_json::to_value(tc).unwrap_or_default();
            }
            if let Some(id) = &self.tool_call_id {
                obj["tool_call_id"] = serde_json::json!(id);
            }
            obj
        } else {
            serde_json::to_value(self).unwrap_or_default()
        }
    }

    /// Convert to Anthropic JSON format (handles image content blocks)
    pub fn to_anthropic_content(&self) -> serde_json::Value {
        if self.has_images() && self.role == "user" {
            let images = self.images.as_deref().unwrap_or_default();
            let mut content_parts: Vec<serde_json::Value> = Vec::new();
            if let Some(text) = &self.content {
                content_parts.push(serde_json::json!({"type": "text", "text": text}));
            }
            for img in images {
                content_parts.push(serde_json::json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": img.media_type,
                        "data": img.data
                    }
                }));
            }
            serde_json::json!(content_parts)
        } else {
            serde_json::json!(self.content.as_deref().unwrap_or(""))
        }
    }
}

impl Message {
    pub fn system(content: &str) -> Self {
        Self {
            role: "system".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            images: None,
        }
    }

    pub fn user(content: &str) -> Self {
        Self {
            role: "user".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            images: None,
        }
    }

    /// Create a user message with image attachments
    pub fn user_with_images(content: &str, images: Vec<ImageAttachment>) -> Self {
        Self {
            role: "user".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            images: if images.is_empty() {
                None
            } else {
                Some(images)
            },
        }
    }

    pub fn assistant_text(content: &str) -> Self {
        Self {
            role: "assistant".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            images: None,
        }
    }

    pub fn assistant_tool_calls(tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
            images: None,
        }
    }

    pub fn tool_result(tool_call_id: &str, content: &str) -> Self {
        Self {
            role: "tool".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
            images: None,
        }
    }
}

// --- Tool Calling Types ---

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Serialize, Debug, Clone)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionSchema,
}

#[derive(Serialize, Debug, Clone)]
pub struct FunctionSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

// --- API Response Types ---

#[derive(Deserialize, Debug)]
pub struct ChatResponse {
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Choice {
    pub message: Message,
    pub finish_reason: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

// --- SSE Streaming Types (OpenAI-compatible) ---

#[derive(Deserialize, Debug)]
pub struct StreamDelta {
    pub choices: Vec<StreamChoice>,
    pub usage: Option<Usage>,
}

#[derive(Deserialize, Debug)]
pub struct StreamChoice {
    pub delta: DeltaContent,
    pub finish_reason: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
pub struct DeltaContent {
    pub content: Option<String>,
    pub tool_calls: Option<Vec<StreamToolCallDelta>>,
}

#[derive(Deserialize, Debug)]
pub struct StreamToolCallDelta {
    pub index: Option<usize>,
    pub id: Option<String>,
    pub function: Option<StreamFunctionDelta>,
}

#[derive(Deserialize, Debug)]
pub struct StreamFunctionDelta {
    pub name: Option<String>,
    pub arguments: Option<String>,
}

// --- Anthropic API Types ---

#[derive(Serialize, Debug)]
pub struct AnthropicRequest {
    pub model: String,
    pub max_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub messages: Vec<AnthropicMessage>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<AnthropicToolDef>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AnthropicMessage {
    pub role: String,
    pub content: serde_json::Value, // string or array of content blocks
}

#[derive(Serialize, Debug)]
pub struct AnthropicToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Deserialize, Debug)]
pub struct AnthropicResponse {
    pub content: Vec<AnthropicContentBlock>,
    pub usage: Option<AnthropicUsage>,
    pub stop_reason: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
}

#[derive(Deserialize, Debug, Clone)]
pub struct AnthropicUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

// --- Anthropic SSE Event Types ---

#[derive(Deserialize, Debug)]
pub struct AnthropicStreamContentDelta {
    #[serde(rename = "type")]
    pub delta_type: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub partial_json: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct AnthropicStreamMessageDelta {
    pub stop_reason: Option<String>,
}

// --- Streaming Chunk (unified across providers) ---

#[derive(Debug)]
pub enum StreamChunk {
    Text(String),
    ToolCallStart { id: String, name: String },
    ToolCallDelta { arguments: String },
    Done,
    Error(String),
}

// --- Persistence Types ---

/// Global CLI config: ~/.bfcode/config.json
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GlobalConfig {
    pub model: String,
    pub temperature: f64,
    pub system_prompt: String,
    #[serde(default)]
    pub provider: Provider,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            model: "grok-4-1-fast".into(),
            temperature: 0.0,
            system_prompt: SYSTEM_PROMPT.into(),
            provider: Provider::Grok,
        }
    }
}

/// Project-local session: .bfcode/sessions/{id}.json
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ProjectSession {
    pub id: String,
    pub title: String,
    pub conversation: Vec<Message>,
    pub total_tokens: u64,
    pub created_at: String,
    pub updated_at: String,
}

impl ProjectSession {
    pub fn new() -> Self {
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let id = format!("{}", chrono::Local::now().format("%Y%m%d_%H%M%S"));
        Self {
            id,
            title: "New session".into(),
            conversation: vec![],
            total_tokens: 0,
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

/// Memory type for context memories (like Claude Code's memory system)
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MemoryType {
    User,
    Feedback,
    Project,
    Reference,
}

impl std::fmt::Display for MemoryType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryType::User => write!(f, "user"),
            MemoryType::Feedback => write!(f, "feedback"),
            MemoryType::Project => write!(f, "project"),
            MemoryType::Reference => write!(f, "reference"),
        }
    }
}

/// A context memory entry stored as a markdown file in .bfcode/memory/
/// Frontmatter is JSON, body is markdown content.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ContextMemory {
    pub name: String,
    pub description: String,
    #[serde(rename = "type")]
    pub memory_type: MemoryType,
    pub content: String,
}

/// File snapshot for undo/revert
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FileSnapshot {
    pub path: String,
    pub original_content: String,
    pub timestamp: String,
    pub message_index: usize,
}

pub const SYSTEM_PROMPT: &str = r#"You are bfcode (back to the future code), a coding assistant running in the user's terminal.

# Tools Available
- read: Read file contents with line numbers. Supports offset/limit for large files.
- write: Create or overwrite a file with new content.
- edit: Modify a file by replacing a specific string with new content. You must provide the exact old_string to match.
- apply_patch: Apply a unified diff patch to one or more files. Use standard unified diff format.
- bash: Run a shell command and return stdout/stderr. Default timeout 120s.
- glob: Find files matching a glob pattern (e.g. "**/*.rs", "src/**/*.ts").
- grep: Search file contents with regex pattern. Returns matching lines with line numbers.
- list_files: List files and directories at a path.
- webfetch: Fetch content from a URL. HTML is auto-stripped to plain text. Use to read docs, web pages, or API responses.
- websearch: Search the web using a search API. Provide a query and optional num_results (default 5). Requires BRAVE_API_KEY or TAVILY_API_KEY env var.
- memory_save: Save a context memory as a markdown file in .bfcode/memory/. Provide name (used as filename slug), description (one-line summary), memory_type (user|feedback|project|reference), and content (markdown body). Use this to remember important context across sessions.
- memory_delete: Delete a context memory by name. Provide the name used when saving.
- memory_list: List all saved context memories with their descriptions.
- memory_search: Search memories semantically using TF-IDF. Provide query and optional top_k (default 5). Returns most relevant memories.
- pdf_read: Read text content from a PDF file. Provide path and optional pages range (e.g. "1-5").
- image_generate: Generate an image using DALL-E API. Provide prompt, optional size (default "1024x1024"), optional output_path. Requires OPENAI_API_KEY.
- tts: Convert text to speech. Provide text, optional voice, optional output_path. Uses system TTS (say/espeak) or OpenAI TTS API with OPENAI_API_KEY.
- browser_navigate: Navigate a headless browser to a URL and return page content as text.
- browser_screenshot: Take a screenshot of the current browser page. Optional output_path.
- browser_click: Click an element by CSS selector in the browser.
- browser_type: Type text into a form element by CSS selector in the browser.
- browser_evaluate: Evaluate JavaScript in the browser and return the result.
- browser_close: Close the headless browser.
- multiedit: Apply multiple find-and-replace edits to a single file in one atomic operation. Provide path and an array of edits (each with old_string, new_string, optional replace_all). Edits are applied sequentially.
- batch: Execute multiple independent tool calls in parallel (up to 25). Provide tool_calls array with tool name and parameters. Cannot nest batch calls. Great for parallel reads, searches, or independent edits.
- task: Launch a subagent to handle a complex multi-step task autonomously. Provide description (3-5 words), prompt (full task), and optional subagent_type. The subagent runs in a separate session with its own context.
- todowrite: Write/update the session todo list. Provide todos array with content, status (pending/in_progress/completed/cancelled), and priority (high/medium/low). Replaces the entire todo list.
- todoread: Read the current session todo list. Returns all todos with their status and priority.
- plan_enter: Enter plan mode (read-only). In plan mode, write/edit/apply_patch tools are disabled. Only reads, searches, and writing to .bfcode/plans/ are allowed. Use this to create a detailed plan before implementation.
- plan_exit: Exit plan mode and return to build mode where all tools are available.

# Guidelines
1. Before modifying files, ALWAYS read them first to understand the current state.
2. Prefer edit over write when making changes to existing files — only replace what needs to change.
3. For large multi-file changes, prefer apply_patch with unified diffs.
4. Explain your plan briefly before making changes.
5. Use bash for compilation, testing, git operations, installing packages, etc.
6. Use glob to discover project structure before diving into specific files.
7. Use grep to find specific code patterns, function definitions, or usages.
8. Keep responses concise but helpful.
9. When asked to do something, use your tools to actually do it — don't just describe what to do.
10. After writing or editing files, briefly confirm what changed.
11. Do not add unnecessary comments, docstrings, or type annotations to code you didn't change.
12. Use websearch to find current information from the web when needed.
13. Use memory_search to find relevant context from saved memories.
14. Use browser tools for interactive web automation tasks."#;

/// Instruction file names to search for (like opencode's AGENTS.md / CLAUDE.md)
pub const INSTRUCTION_FILES: &[&str] = &[
    "AGENTS.md",
    "CLAUDE.md",
    "BFCODE.md",
    ".bfcode/instructions.md",
];

// --- Tool Argument Types ---

#[derive(Deserialize, Debug)]
pub struct ReadArgs {
    pub path: String,
    #[serde(default)]
    pub offset: Option<u64>,
    #[serde(default)]
    pub limit: Option<u64>,
}

#[derive(Deserialize, Debug)]
pub struct WriteArgs {
    pub path: String,
    pub content: String,
}

#[derive(Deserialize, Debug)]
pub struct EditArgs {
    pub path: String,
    pub old_string: String,
    pub new_string: String,
    #[serde(default)]
    pub replace_all: Option<bool>,
}

#[derive(Deserialize, Debug)]
pub struct BashArgs {
    pub command: String,
    #[serde(default)]
    pub timeout: Option<u64>,
}

#[derive(Deserialize, Debug)]
pub struct GlobArgs {
    pub pattern: String,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct GrepArgs {
    pub pattern: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub include: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct ListFilesArgs {
    pub path: String,
}

#[derive(Deserialize, Debug)]
pub struct ApplyPatchArgs {
    pub patch: String,
}

#[derive(Deserialize, Debug)]
pub struct WebFetchArgs {
    pub url: String,
}

#[derive(Deserialize, Debug)]
pub struct MemorySaveArgs {
    pub name: String,
    pub description: String,
    pub memory_type: MemoryType,
    pub content: String,
}

#[derive(Deserialize, Debug)]
pub struct MemoryDeleteArgs {
    pub name: String,
}

#[derive(Deserialize, Debug)]
pub struct MemorySearchArgs {
    pub query: String,
    #[serde(default)]
    pub top_k: Option<usize>,
}

#[derive(Deserialize, Debug)]
pub struct WebSearchArgs {
    pub query: String,
    #[serde(default)]
    pub num_results: Option<u32>,
}

#[derive(Deserialize, Debug)]
pub struct PdfReadArgs {
    pub path: String,
    #[serde(default)]
    pub pages: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct ImageGenerateArgs {
    pub prompt: String,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub output_path: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct TtsArgs {
    pub text: String,
    #[serde(default)]
    pub voice: Option<String>,
    #[serde(default)]
    pub output_path: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct BrowserNavigateArgs {
    pub url: String,
}

#[derive(Deserialize, Debug)]
pub struct BrowserScreenshotArgs {
    #[serde(default)]
    pub output_path: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct BrowserClickArgs {
    pub selector: String,
}

#[derive(Deserialize, Debug)]
pub struct BrowserTypeArgs {
    pub selector: String,
    pub text: String,
}

#[derive(Deserialize, Debug)]
pub struct BrowserEvaluateArgs {
    pub script: String,
}

// --- Multi-Edit Tool Args ---

#[derive(Deserialize, Debug)]
pub struct MultiEditArgs {
    pub path: String,
    pub edits: Vec<SingleEdit>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct SingleEdit {
    pub old_string: String,
    pub new_string: String,
    #[serde(default)]
    pub replace_all: Option<bool>,
}

// --- Batch Tool Args ---

#[derive(Deserialize, Debug)]
pub struct BatchArgs {
    pub tool_calls: Vec<BatchToolCall>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct BatchToolCall {
    pub tool: String,
    pub parameters: serde_json::Value,
}

// --- Task/Subagent Tool Args ---

#[derive(Deserialize, Debug)]
pub struct TaskToolArgs {
    pub description: String,
    pub prompt: String,
    #[serde(default)]
    pub subagent_type: Option<String>,
}

// --- Todo Tool Args ---

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
    #[serde(default = "default_priority")]
    pub priority: TodoPriority,
}

fn default_priority() -> TodoPriority {
    TodoPriority::Medium
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

impl std::fmt::Display for TodoStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TodoStatus::Pending => write!(f, "pending"),
            TodoStatus::InProgress => write!(f, "in_progress"),
            TodoStatus::Completed => write!(f, "completed"),
            TodoStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TodoPriority {
    High,
    Medium,
    Low,
}

impl std::fmt::Display for TodoPriority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TodoPriority::High => write!(f, "high"),
            TodoPriority::Medium => write!(f, "medium"),
            TodoPriority::Low => write!(f, "low"),
        }
    }
}

#[derive(Deserialize, Debug)]
pub struct TodoWriteArgs {
    pub todos: Vec<TodoItem>,
}

// --- Plan Mode Args ---

#[derive(Deserialize, Debug)]
pub struct PlanEnterArgs {
    #[serde(default)]
    pub plan_name: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct PlanExitArgs {}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Provider tests ---

    #[test]
    fn test_detect_provider_grok() {
        assert_eq!(detect_provider("grok-4-1-fast"), Provider::Grok);
        assert_eq!(detect_provider("grok-3"), Provider::Grok);
    }

    #[test]
    fn test_detect_provider_openai() {
        assert_eq!(detect_provider("gpt-4o"), Provider::OpenAI);
        assert_eq!(detect_provider("gpt-4o-mini"), Provider::OpenAI);
        assert_eq!(detect_provider("o1-preview"), Provider::OpenAI);
        assert_eq!(detect_provider("o3-mini"), Provider::OpenAI);
        assert_eq!(detect_provider("o4-mini"), Provider::OpenAI);
    }

    #[test]
    fn test_detect_provider_anthropic() {
        assert_eq!(
            detect_provider("claude-sonnet-4-20250514"),
            Provider::Anthropic
        );
        assert_eq!(
            detect_provider("claude-3-5-haiku-latest"),
            Provider::Anthropic
        );
    }

    #[test]
    fn test_detect_provider_unknown_defaults_to_grok() {
        assert_eq!(detect_provider("some-custom-model"), Provider::Grok);
        assert_eq!(detect_provider("llama-3"), Provider::Grok);
    }

    #[test]
    fn test_provider_configs_has_all_three() {
        let configs = provider_configs();
        assert_eq!(configs.len(), 3);
        assert!(configs.iter().any(|c| c.provider == Provider::Grok));
        assert!(configs.iter().any(|c| c.provider == Provider::OpenAI));
        assert!(configs.iter().any(|c| c.provider == Provider::Anthropic));
    }

    #[test]
    fn test_provider_serialization() {
        let json = serde_json::to_string(&Provider::Grok).unwrap();
        assert_eq!(json, r#""grok""#);
        let parsed: Provider = serde_json::from_str(r#""anthropic""#).unwrap();
        assert_eq!(parsed, Provider::Anthropic);
    }

    #[test]
    fn test_context_limit_for_model() {
        assert_eq!(context_limit_for_model("grok-4-1-fast"), 131_072);
        assert_eq!(context_limit_for_model("gpt-4o"), 128_000);
        assert_eq!(context_limit_for_model("claude-sonnet-4-20250514"), 200_000);
    }

    // --- Existing tests ---

    #[test]
    fn test_message_system() {
        let msg = Message::system("hello");
        assert_eq!(msg.role, "system");
        assert_eq!(msg.content.as_deref(), Some("hello"));
        assert!(msg.tool_calls.is_none());
        assert!(msg.tool_call_id.is_none());
    }

    #[test]
    fn test_message_user() {
        let msg = Message::user("test input");
        assert_eq!(msg.role, "user");
        assert_eq!(msg.content.as_deref(), Some("test input"));
    }

    #[test]
    fn test_message_assistant_text() {
        let msg = Message::assistant_text("response");
        assert_eq!(msg.role, "assistant");
        assert_eq!(msg.content.as_deref(), Some("response"));
        assert!(msg.tool_calls.is_none());
    }

    #[test]
    fn test_message_assistant_tool_calls() {
        let tc = ToolCall {
            id: "call_1".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "read".into(),
                arguments: r#"{"path":"foo.txt"}"#.into(),
            },
        };
        let msg = Message::assistant_tool_calls(vec![tc]);
        assert_eq!(msg.role, "assistant");
        assert!(msg.content.is_none());
        assert_eq!(msg.tool_calls.as_ref().map(|v| v.len()), Some(1));
    }

    #[test]
    fn test_message_tool_result() {
        let msg = Message::tool_result("call_1", "file contents");
        assert_eq!(msg.role, "tool");
        assert_eq!(msg.content.as_deref(), Some("file contents"));
        assert_eq!(msg.tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn test_message_serialization_skips_none() {
        let msg = Message::user("hi");
        let json = serde_json::to_string(&msg).unwrap();
        assert!(!json.contains("tool_calls"));
        assert!(!json.contains("tool_call_id"));
    }

    #[test]
    fn test_message_roundtrip() {
        let msg = Message::user("roundtrip test");
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.role, "user");
        assert_eq!(parsed.content.as_deref(), Some("roundtrip test"));
    }

    #[test]
    fn test_global_config_default() {
        let config = GlobalConfig::default();
        assert_eq!(config.model, "grok-4-1-fast");
        assert_eq!(config.temperature, 0.0);
        assert!(!config.system_prompt.is_empty());
        assert!(config.system_prompt.contains("bfcode"));
        assert_eq!(config.provider, Provider::Grok);
    }

    #[test]
    fn test_global_config_serialization() {
        let config = GlobalConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let parsed: GlobalConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.model, config.model);
        assert_eq!(parsed.temperature, config.temperature);
        assert_eq!(parsed.provider, Provider::Grok);
    }

    #[test]
    fn test_global_config_backward_compat() {
        // Old config without provider field should still deserialize
        let json = r#"{"model":"grok-4-1-fast","temperature":0.0,"system_prompt":"test"}"#;
        let parsed: GlobalConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.provider, Provider::Grok); // default
    }

    #[test]
    fn test_project_session_new() {
        let session = ProjectSession::new();
        assert!(!session.id.is_empty());
        assert_eq!(session.title, "New session");
        assert!(session.conversation.is_empty());
        assert_eq!(session.total_tokens, 0);
        assert!(!session.created_at.is_empty());
        assert_eq!(session.created_at, session.updated_at);
    }

    #[test]
    fn test_project_session_serialization() {
        let mut session = ProjectSession::new();
        session.conversation.push(Message::system("sys"));
        session.conversation.push(Message::user("hello"));
        session.total_tokens = 42;

        let json = serde_json::to_string(&session).unwrap();
        let parsed: ProjectSession = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, session.id);
        assert_eq!(parsed.conversation.len(), 2);
        assert_eq!(parsed.total_tokens, 42);
    }

    #[test]
    fn test_chat_request_serialization() {
        let req = ChatRequest {
            model: "grok-4-1-fast".into(),
            messages: vec![Message::user("hi")],
            stream: false,
            temperature: 0.0,
            tools: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("grok-4-1-fast"));
        assert!(!json.contains("tools")); // None should be skipped
    }

    #[test]
    fn test_chat_response_deserialization() {
        let json = r#"{
            "choices": [{
                "message": {"role": "assistant", "content": "Hello!"},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        }"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(resp.choices[0].message.content.as_deref(), Some("Hello!"));
        assert_eq!(resp.usage.as_ref().map(|u| u.total_tokens), Some(15));
    }

    #[test]
    fn test_chat_response_without_usage() {
        let json = r#"{"choices": [{"message": {"role": "assistant", "content": "hi"}, "finish_reason": null}]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.usage.is_none());
    }

    #[test]
    fn test_tool_call_deserialization() {
        let json = r#"{
            "id": "call_abc",
            "type": "function",
            "function": {"name": "bash", "arguments": "{\"command\": \"ls\"}"}
        }"#;
        let tc: ToolCall = serde_json::from_str(json).unwrap();
        assert_eq!(tc.id, "call_abc");
        assert_eq!(tc.call_type, "function");
        assert_eq!(tc.function.name, "bash");
    }

    #[test]
    fn test_read_args_minimal() {
        let json = r#"{"path": "foo.rs"}"#;
        let args: ReadArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.path, "foo.rs");
        assert!(args.offset.is_none());
        assert!(args.limit.is_none());
    }

    #[test]
    fn test_read_args_full() {
        let json = r#"{"path": "foo.rs", "offset": 10, "limit": 50}"#;
        let args: ReadArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.offset, Some(10));
        assert_eq!(args.limit, Some(50));
    }

    #[test]
    fn test_edit_args() {
        let json =
            r#"{"path": "f.rs", "old_string": "foo", "new_string": "bar", "replace_all": true}"#;
        let args: EditArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.replace_all, Some(true));
    }

    #[test]
    fn test_instruction_files_not_empty() {
        assert!(!INSTRUCTION_FILES.is_empty());
        assert!(INSTRUCTION_FILES.contains(&"CLAUDE.md"));
        assert!(INSTRUCTION_FILES.contains(&"BFCODE.md"));
    }

    #[test]
    fn test_file_snapshot_serialization() {
        let snap = FileSnapshot {
            path: "src/main.rs".into(),
            original_content: "fn main() {}".into(),
            timestamp: "20260321_120000".into(),
            message_index: 5,
        };
        let json = serde_json::to_string(&snap).unwrap();
        let parsed: FileSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.path, "src/main.rs");
        assert_eq!(parsed.message_index, 5);
    }

    #[test]
    fn test_stream_delta_deserialization() {
        let json = r#"{
            "choices": [{
                "delta": {"content": "Hello"},
                "finish_reason": null
            }]
        }"#;
        let delta: StreamDelta = serde_json::from_str(json).unwrap();
        assert_eq!(delta.choices[0].delta.content.as_deref(), Some("Hello"));
    }

    #[test]
    fn test_stream_delta_tool_call() {
        let json = r#"{
            "choices": [{
                "delta": {
                    "tool_calls": [{"index": 0, "id": "call_1", "function": {"name": "read", "arguments": ""}}]
                },
                "finish_reason": null
            }]
        }"#;
        let delta: StreamDelta = serde_json::from_str(json).unwrap();
        let tcs = delta.choices[0].delta.tool_calls.as_ref().unwrap();
        assert_eq!(tcs[0].id.as_deref(), Some("call_1"));
    }

    #[test]
    fn test_anthropic_content_block_text() {
        let json = r#"{"type": "text", "text": "Hello world"}"#;
        let block: AnthropicContentBlock = serde_json::from_str(json).unwrap();
        match block {
            AnthropicContentBlock::Text { text } => assert_eq!(text, "Hello world"),
            _ => panic!("Expected text block"),
        }
    }

    #[test]
    fn test_anthropic_content_block_tool_use() {
        let json =
            r#"{"type": "tool_use", "id": "tu_1", "name": "read", "input": {"path": "foo.rs"}}"#;
        let block: AnthropicContentBlock = serde_json::from_str(json).unwrap();
        match block {
            AnthropicContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "tu_1");
                assert_eq!(name, "read");
                assert_eq!(input["path"], "foo.rs");
            }
            _ => panic!("Expected tool_use block"),
        }
    }

    #[test]
    fn test_anthropic_response_deserialization() {
        let json = r#"{
            "content": [{"type": "text", "text": "Hello!"}],
            "usage": {"input_tokens": 10, "output_tokens": 5},
            "stop_reason": "end_turn"
        }"#;
        let resp: AnthropicResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.content.len(), 1);
        assert_eq!(resp.usage.as_ref().unwrap().input_tokens, 10);
    }

    #[test]
    fn test_apply_patch_args() {
        let json = r#"{"patch": "--- a/foo.rs\n+++ b/foo.rs\n@@ -1 +1 @@\n-old\n+new"}"#;
        let args: ApplyPatchArgs = serde_json::from_str(json).unwrap();
        assert!(args.patch.contains("foo.rs"));
    }

    #[test]
    fn test_system_prompt_includes_apply_patch() {
        assert!(SYSTEM_PROMPT.contains("apply_patch"));
    }

    #[test]
    fn test_system_prompt_includes_webfetch() {
        assert!(SYSTEM_PROMPT.contains("webfetch"));
    }

    #[test]
    fn test_system_prompt_includes_memory_tools() {
        assert!(SYSTEM_PROMPT.contains("memory_save"));
        assert!(SYSTEM_PROMPT.contains("memory_delete"));
        assert!(SYSTEM_PROMPT.contains("memory_list"));
    }

    // --- Cost tracking tests ---

    #[test]
    fn test_model_cost_grok() {
        let cost = model_cost("grok-4-1-fast");
        assert!(cost.input_per_million > 0.0);
        assert!(cost.output_per_million > 0.0);
    }

    #[test]
    fn test_model_cost_openai() {
        let cost = model_cost("gpt-4o");
        assert_eq!(cost.input_per_million, 2.5);
        assert_eq!(cost.output_per_million, 10.0);
    }

    #[test]
    fn test_model_cost_openai_mini() {
        let cost = model_cost("gpt-4o-mini");
        assert_eq!(cost.input_per_million, 0.15);
    }

    #[test]
    fn test_model_cost_anthropic_sonnet() {
        let cost = model_cost("claude-sonnet-4-20250514");
        assert_eq!(cost.input_per_million, 3.0);
        assert_eq!(cost.output_per_million, 15.0);
    }

    #[test]
    fn test_model_cost_anthropic_opus() {
        let cost = model_cost("claude-opus-4");
        assert_eq!(cost.input_per_million, 15.0);
        assert_eq!(cost.output_per_million, 75.0);
    }

    #[test]
    fn test_model_cost_anthropic_haiku() {
        let cost = model_cost("claude-haiku-3-5");
        assert_eq!(cost.input_per_million, 0.25);
    }

    #[test]
    fn test_calculate_cost() {
        let cost = calculate_cost("gpt-4o", 1_000_000, 1_000_000);
        assert!((cost - 12.5).abs() < 0.01); // $2.50 input + $10 output
    }

    #[test]
    fn test_calculate_cost_small() {
        let cost = calculate_cost("gpt-4o", 1000, 500);
        assert!(cost > 0.0);
        assert!(cost < 0.01);
    }

    #[test]
    fn test_format_cost_small() {
        assert_eq!(format_cost(0.0025), "$0.0025");
    }

    #[test]
    fn test_format_cost_large() {
        assert_eq!(format_cost(1.50), "$1.50");
    }

    // --- Protected files tests ---

    #[test]
    fn test_protected_file_patterns_not_empty() {
        assert!(!PROTECTED_FILE_PATTERNS.is_empty());
    }

    #[test]
    fn test_protected_file_patterns_includes_env() {
        assert!(PROTECTED_FILE_PATTERNS.contains(&".env"));
        assert!(PROTECTED_FILE_PATTERNS.contains(&".env.local"));
    }

    #[test]
    fn test_protected_file_patterns_includes_keys() {
        assert!(PROTECTED_FILE_PATTERNS.contains(&".pem"));
        assert!(PROTECTED_FILE_PATTERNS.contains(&".key"));
        assert!(PROTECTED_FILE_PATTERNS.contains(&"id_rsa"));
    }

    // --- Image attachment tests ---

    #[test]
    fn test_image_attachment_serialization() {
        let img = ImageAttachment {
            data: "base64data".into(),
            media_type: "image/png".into(),
        };
        let json = serde_json::to_string(&img).unwrap();
        assert!(json.contains("base64data"));
        assert!(json.contains("image/png"));
    }

    #[test]
    fn test_message_user_with_images() {
        let img = ImageAttachment {
            data: "abc".into(),
            media_type: "image/jpeg".into(),
        };
        let msg = Message::user_with_images("describe this", vec![img]);
        assert_eq!(msg.role, "user");
        assert!(msg.has_images());
        assert_eq!(msg.images.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_message_user_with_no_images() {
        let msg = Message::user_with_images("hello", vec![]);
        assert!(!msg.has_images());
        assert!(msg.images.is_none());
    }

    #[test]
    fn test_message_has_images_false_for_normal() {
        let msg = Message::user("hello");
        assert!(!msg.has_images());
    }

    #[test]
    fn test_message_to_openai_json_no_images() {
        let msg = Message::user("hello");
        let json = msg.to_openai_json();
        assert_eq!(json["content"], "hello");
    }

    #[test]
    fn test_message_to_openai_json_with_images() {
        let img = ImageAttachment {
            data: "AAAA".into(),
            media_type: "image/png".into(),
        };
        let msg = Message::user_with_images("what is this?", vec![img]);
        let json = msg.to_openai_json();
        let content = json["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
        assert!(
            content[1]["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
    }

    #[test]
    fn test_message_to_anthropic_content_no_images() {
        let msg = Message::user("hello");
        let content = msg.to_anthropic_content();
        assert_eq!(content, "hello");
    }

    #[test]
    fn test_message_to_anthropic_content_with_images() {
        let img = ImageAttachment {
            data: "BBBB".into(),
            media_type: "image/jpeg".into(),
        };
        let msg = Message::user_with_images("describe", vec![img]);
        let content = msg.to_anthropic_content();
        let arr = content.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["type"], "text");
        assert_eq!(arr[1]["type"], "image");
        assert_eq!(arr[1]["source"]["type"], "base64");
        assert_eq!(arr[1]["source"]["media_type"], "image/jpeg");
    }

    #[test]
    fn test_message_serialization_skips_images_none() {
        let msg = Message::user("hi");
        let json = serde_json::to_string(&msg).unwrap();
        assert!(!json.contains("images"));
    }

    // --- MemoryType tests ---

    #[test]
    fn test_memory_type_display() {
        assert_eq!(MemoryType::User.to_string(), "user");
        assert_eq!(MemoryType::Feedback.to_string(), "feedback");
        assert_eq!(MemoryType::Project.to_string(), "project");
        assert_eq!(MemoryType::Reference.to_string(), "reference");
    }

    #[test]
    fn test_webfetch_args() {
        let json = r#"{"url": "https://example.com"}"#;
        let args: WebFetchArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.url, "https://example.com");
    }
}
