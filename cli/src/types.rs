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
pub fn get_provider_config(provider: &Provider) -> ProviderConfig {
    provider_configs()
        .into_iter()
        .find(|c| c.provider == *provider)
        .unwrap()
}

/// Get context limit for a model
pub fn context_limit_for_model(model: &str) -> u64 {
    let provider = detect_provider(model);
    get_provider_config(&provider).context_limit
}

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
}

impl Message {
    pub fn system(content: &str) -> Self {
        Self {
            role: "system".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn user(content: &str) -> Self {
        Self {
            role: "user".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant_text(content: &str) -> Self {
        Self {
            role: "assistant".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant_tool_calls(tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
        }
    }

    pub fn tool_result(tool_call_id: &str, content: &str) -> Self {
        Self {
            role: "tool".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
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
- memory_save: Save a context memory as a markdown file in .bfcode/memory/. Provide name (used as filename slug), description (one-line summary), memory_type (user|feedback|project|reference), and content (markdown body). Use this to remember important context across sessions.
- memory_delete: Delete a context memory by name. Provide the name used when saving.
- memory_list: List all saved context memories with their descriptions.

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
11. Do not add unnecessary comments, docstrings, or type annotations to code you didn't change."#;

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
        assert_eq!(detect_provider("claude-sonnet-4-20250514"), Provider::Anthropic);
        assert_eq!(detect_provider("claude-3-5-haiku-latest"), Provider::Anthropic);
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
        let json = r#"{"path": "f.rs", "old_string": "foo", "new_string": "bar", "replace_all": true}"#;
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
        let json = r#"{"type": "tool_use", "id": "tu_1", "name": "read", "input": {"path": "foo.rs"}}"#;
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
}
