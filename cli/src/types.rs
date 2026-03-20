use serde::{Deserialize, Serialize};

// --- Chat Completion Request ---

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

// --- Persistence Types ---

/// Global CLI config: ~/.bfcode/config.json
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GlobalConfig {
    pub model: String,
    pub temperature: f64,
    pub system_prompt: String,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            model: "grok-4-1-fast".into(),
            temperature: 0.0,
            system_prompt: SYSTEM_PROMPT.into(),
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

pub const SYSTEM_PROMPT: &str = r#"You are bfcode (back to the future code), a coding assistant running in the user's terminal.

# Tools Available
- read: Read file contents with line numbers. Supports offset/limit for large files.
- write: Create or overwrite a file with new content.
- edit: Modify a file by replacing a specific string with new content. You must provide the exact old_string to match.
- bash: Run a shell command and return stdout/stderr. Default timeout 120s.
- glob: Find files matching a glob pattern (e.g. "**/*.rs", "src/**/*.ts").
- grep: Search file contents with regex pattern. Returns matching lines with line numbers.
- list_files: List files and directories at a path.

# Guidelines
1. Before modifying files, ALWAYS read them first to understand the current state.
2. Prefer edit over write when making changes to existing files — only replace what needs to change.
3. Explain your plan briefly before making changes.
4. Use bash for compilation, testing, git operations, installing packages, etc.
5. Use glob to discover project structure before diving into specific files.
6. Use grep to find specific code patterns, function definitions, or usages.
7. Keep responses concise but helpful.
8. When asked to do something, use your tools to actually do it — don't just describe what to do.
9. After writing or editing files, briefly confirm what changed.
10. Do not add unnecessary comments, docstrings, or type annotations to code you didn't change."#;

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

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[test]
    fn test_global_config_serialization() {
        let config = GlobalConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let parsed: GlobalConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.model, config.model);
        assert_eq!(parsed.temperature, config.temperature);
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
}
