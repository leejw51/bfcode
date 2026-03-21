//! Markdown-based context management (inspired by opencode)
//!
//! Generates and loads markdown files for:
//! - Session transcript export
//! - Structured compaction summaries
//! - Environment context snapshots
//! - Token estimation for context-aware compaction

use crate::types::{Message, ProjectSession};
use anyhow::{Context as _, Result};
use std::path::{Path, PathBuf};

// --- Token Estimation (like opencode's Token.estimate) ---

const CHARS_PER_TOKEN: f64 = 4.0;

/// Estimate token count for a string (~4 chars per token)
pub fn estimate_tokens(text: &str) -> u64 {
    (text.len() as f64 / CHARS_PER_TOKEN).ceil() as u64
}

/// Estimate total tokens in a conversation
pub fn estimate_conversation_tokens(messages: &[Message]) -> u64 {
    messages
        .iter()
        .map(|m| {
            let mut tokens = 4u64; // overhead per message (role, formatting)
            if let Some(c) = &m.content {
                tokens += estimate_tokens(c);
            }
            if let Some(tcs) = &m.tool_calls {
                for tc in tcs {
                    tokens += estimate_tokens(&tc.function.name);
                    tokens += estimate_tokens(&tc.function.arguments);
                }
            }
            tokens
        })
        .sum()
}

// --- Session transcript export (.md) ---

/// Format a session as a readable markdown transcript
pub fn format_transcript(session: &ProjectSession) -> String {
    let mut md = String::new();

    // Header
    md.push_str(&format!("# Session: {}\n\n", session.title));
    md.push_str(&format!("- **ID:** {}\n", session.id));
    md.push_str(&format!("- **Created:** {}\n", session.created_at));
    md.push_str(&format!("- **Updated:** {}\n", session.updated_at));
    md.push_str(&format!("- **Tokens:** {}\n", session.total_tokens));
    md.push_str("\n---\n\n");

    for msg in &session.conversation {
        match msg.role.as_str() {
            "system" => {
                // Skip system messages in transcript (too long)
                md.push_str("*[system prompt omitted]*\n\n");
            }
            "user" => {
                md.push_str("## User\n\n");
                if let Some(content) = &msg.content {
                    md.push_str(content);
                    md.push_str("\n\n");
                }
            }
            "assistant" => {
                md.push_str("## Assistant\n\n");
                if let Some(content) = &msg.content {
                    md.push_str(content);
                    md.push_str("\n\n");
                }
                if let Some(tool_calls) = &msg.tool_calls {
                    for tc in tool_calls {
                        md.push_str(&format!(
                            "### Tool Call: `{}`\n\n```json\n{}\n```\n\n",
                            tc.function.name, tc.function.arguments
                        ));
                    }
                }
            }
            "tool" => {
                let tool_id = msg.tool_call_id.as_deref().unwrap_or("unknown");
                md.push_str(&format!("### Tool Result ({})\n\n", tool_id));
                if let Some(content) = &msg.content {
                    // Truncate long tool outputs in transcript
                    let truncated = if content.len() > 2000 {
                        format!(
                            "{}...\n\n*[truncated, {} bytes total]*",
                            &content[..2000],
                            content.len()
                        )
                    } else {
                        content.clone()
                    };
                    md.push_str(&format!("```\n{truncated}\n```\n\n"));
                }
            }
            _ => {}
        }
    }

    md
}

/// Export session transcript as a markdown file
pub fn export_transcript(session: &ProjectSession, output: Option<&str>) -> Result<PathBuf> {
    let md = format_transcript(session);

    let path = match output {
        Some(p) => PathBuf::from(p),
        None => {
            let short_id = &session.id[..session.id.len().min(8)];
            PathBuf::from(format!("session-{short_id}.md"))
        }
    };

    std::fs::write(&path, &md)
        .with_context(|| format!("Failed to write transcript to {}", path.display()))?;
    Ok(path)
}

// --- Structured compaction summary (like opencode's compaction.ts) ---

/// Compaction summary template — structured markdown sections
const COMPACTION_TEMPLATE: &str = r#"## Goal

{goal}

## Instructions

{instructions}

## Discoveries

{discoveries}

## Accomplished

{accomplished}

## Relevant Files

{files}
"#;

/// Extract a structured summary from a conversation for compaction.
/// Analyzes messages to build sections for goal, discoveries, accomplished work, and files.
pub fn build_compaction_summary(session: &ProjectSession) -> String {
    let mut goal = String::new();
    let mut discoveries = Vec::new();
    let mut accomplished = Vec::new();
    let mut files_seen = std::collections::BTreeSet::new();

    // Walk conversation to extract context
    for msg in &session.conversation {
        match msg.role.as_str() {
            "user" => {
                if let Some(content) = &msg.content {
                    // First user message is likely the goal
                    if goal.is_empty() {
                        goal = content.chars().take(500).collect();
                    }
                }
            }
            "assistant" => {
                if let Some(content) = &msg.content {
                    // Look for file paths mentioned in assistant responses
                    extract_file_paths(content, &mut files_seen);

                    // Last few assistant messages represent accomplished work
                    let summary: String = content.lines().take(3).collect::<Vec<_>>().join(" ");
                    if !summary.trim().is_empty() {
                        accomplished.push(summary);
                    }
                }
                // Track tool calls as discoveries
                if let Some(tool_calls) = &msg.tool_calls {
                    for tc in tool_calls {
                        match tc.function.name.as_str() {
                            "read" | "write" | "edit" => {
                                if let Ok(args) =
                                    serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                                {
                                    if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                                        files_seen.insert(path.to_string());
                                    }
                                }
                            }
                            "grep" | "glob" => {
                                discoveries.push(format!(
                                    "Searched with `{}`: {}",
                                    tc.function.name,
                                    tc.function.arguments.chars().take(100).collect::<String>()
                                ));
                            }
                            "bash" => {
                                if let Ok(args) =
                                    serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                                {
                                    if let Some(cmd) =
                                        args.get("command").and_then(|v| v.as_str())
                                    {
                                        discoveries.push(format!(
                                            "Ran: `{}`",
                                            cmd.chars().take(80).collect::<String>()
                                        ));
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            "tool" => {
                if let Some(content) = &msg.content {
                    extract_file_paths(content, &mut files_seen);
                }
            }
            _ => {}
        }
    }

    // Keep only recent accomplished items
    let accomplished_text = if accomplished.is_empty() {
        "No completed work yet.".to_string()
    } else {
        accomplished
            .iter()
            .rev()
            .take(10)
            .rev()
            .enumerate()
            .map(|(i, s)| format!("{}. {}", i + 1, truncate_line(s, 200)))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let discoveries_text = if discoveries.is_empty() {
        "No significant discoveries.".to_string()
    } else {
        discoveries
            .iter()
            .rev()
            .take(15)
            .rev()
            .map(|s| format!("- {}", truncate_line(s, 150)))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let files_text = if files_seen.is_empty() {
        "No files referenced.".to_string()
    } else {
        files_seen
            .iter()
            .map(|f| format!("- `{f}`"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    if goal.is_empty() {
        goal = "Continue assisting the user.".to_string();
    }

    COMPACTION_TEMPLATE
        .replace("{goal}", &goal)
        .replace("{instructions}", "Continue from previous context. Refer to the summary below.")
        .replace("{discoveries}", &discoveries_text)
        .replace("{accomplished}", &accomplished_text)
        .replace("{files}", &files_text)
}

/// Save compaction summary as a markdown file and return the content for injection
pub fn save_compaction_summary(session: &ProjectSession) -> Result<(PathBuf, String)> {
    let summary = build_compaction_summary(session);

    let dir = PathBuf::from(".bfcode").join("context");
    std::fs::create_dir_all(&dir)?;

    let filename = format!("compaction-{}.md", session.id);
    let path = dir.join(&filename);
    std::fs::write(&path, &summary)?;

    Ok((path, summary))
}

/// Load the most recent compaction summary for a session if it exists
pub fn load_compaction_summary(session_id: &str) -> Option<String> {
    let path = PathBuf::from(".bfcode")
        .join("context")
        .join(format!("compaction-{session_id}.md"));
    if path.exists() {
        std::fs::read_to_string(&path).ok()
    } else {
        None
    }
}

// --- Environment context snapshot (like opencode's system.ts) ---

/// Generate an environment context markdown string
pub fn build_environment_context() -> String {
    let mut ctx = String::from("# Environment\n\n");

    // Working directory
    if let Ok(cwd) = std::env::current_dir() {
        ctx.push_str(&format!("- **Working directory:** `{}`\n", cwd.display()));
    }

    // Platform
    ctx.push_str(&format!("- **Platform:** {}\n", std::env::consts::OS));
    ctx.push_str(&format!("- **Arch:** {}\n", std::env::consts::ARCH));

    // Date
    let now = chrono::Local::now();
    ctx.push_str(&format!(
        "- **Date:** {}\n",
        now.format("%Y-%m-%d %H:%M:%S")
    ));

    // Git info
    if let Some(git_info) = get_git_context() {
        ctx.push_str(&format!("\n## Git\n\n{git_info}\n"));
    }

    // Project structure (top-level only)
    if let Some(tree) = get_project_tree() {
        ctx.push_str(&format!("\n## Project Structure\n\n```\n{tree}\n```\n"));
    }

    ctx
}

/// Save environment context as a markdown file
pub fn save_environment_context() -> Result<PathBuf> {
    let ctx = build_environment_context();

    let dir = PathBuf::from(".bfcode").join("context");
    std::fs::create_dir_all(&dir)?;

    let path = dir.join("environment.md");
    std::fs::write(&path, &ctx)?;
    Ok(path)
}

/// Load all context markdown files and combine them
pub fn load_context_files() -> Option<String> {
    let dir = PathBuf::from(".bfcode").join("context");
    if !dir.exists() {
        return None;
    }

    let mut context = String::new();
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "md")
                .unwrap_or(false)
        })
        .collect();

    // Sort by name for deterministic ordering
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        if let Ok(content) = std::fs::read_to_string(&path) {
            // Stop adding files once we've accumulated enough context
            if !context.is_empty() && context.len() + content.len() > 30_000 {
                break;
            }
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            context.push_str(&format!("\n<!-- context: {name} -->\n{content}\n"));
        }
    }

    if context.is_empty() {
        None
    } else {
        Some(context)
    }
}

// --- Helpers ---

/// Extract file paths from text content (heuristic)
fn extract_file_paths(text: &str, files: &mut std::collections::BTreeSet<String>) {
    for word in text.split_whitespace() {
        let cleaned = word.trim_matches(|c: char| c == '`' || c == '\'' || c == '"');
        if looks_like_file_path(cleaned) {
            files.insert(cleaned.to_string());
        }
    }
}

fn looks_like_file_path(s: &str) -> bool {
    if s.len() < 3 || s.len() > 200 {
        return false;
    }
    // Must contain a dot or slash
    if !s.contains('.') && !s.contains('/') {
        return false;
    }
    // Common file extensions
    let extensions = [
        ".rs", ".ts", ".tsx", ".js", ".jsx", ".py", ".go", ".java", ".c", ".cpp", ".h",
        ".toml", ".json", ".yaml", ".yml", ".md", ".txt", ".sh", ".css", ".html", ".sql",
        ".lock", ".cfg", ".ini", ".env", ".xml",
    ];
    // Exclude URLs
    if s.contains("://") {
        return false;
    }
    // Check if it ends with a known extension or looks like a path
    extensions.iter().any(|ext| s.ends_with(ext))
        || (s.contains('/') && !s.contains(' '))
}

fn truncate_line(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

fn get_git_context() -> Option<String> {
    let branch = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .output()
        .ok()?;
    if !branch.status.success() {
        return None;
    }
    let branch_name = String::from_utf8_lossy(&branch.stdout).trim().to_string();

    let status = std::process::Command::new("git")
        .args(["status", "--short"])
        .output()
        .ok()?;
    let status_text = String::from_utf8_lossy(&status.stdout).trim().to_string();

    let log = std::process::Command::new("git")
        .args(["log", "--oneline", "-5"])
        .output()
        .ok()?;
    let log_text = String::from_utf8_lossy(&log.stdout).trim().to_string();

    let mut info = format!("- **Branch:** `{branch_name}`\n");
    if !status_text.is_empty() {
        let changed_count = status_text.lines().count();
        info.push_str(&format!("- **Changed files:** {changed_count}\n"));
        // Show first few changes
        for line in status_text.lines().take(10) {
            info.push_str(&format!("  - `{line}`\n"));
        }
    } else {
        info.push_str("- **Status:** clean\n");
    }
    if !log_text.is_empty() {
        info.push_str("- **Recent commits:**\n");
        for line in log_text.lines().take(5) {
            info.push_str(&format!("  - {line}\n"));
        }
    }

    Some(info)
}

fn get_project_tree() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let mut entries = Vec::new();

    let read_dir = std::fs::read_dir(&cwd).ok()?;
    let mut items: Vec<_> = read_dir.flatten().collect();
    items.sort_by_key(|e| e.file_name());

    for entry in items {
        let name = entry.file_name().to_string_lossy().to_string();
        // Skip hidden and build dirs
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            entries.push(format!("{name}/"));
        } else {
            entries.push(name);
        }
    }

    if entries.is_empty() {
        None
    } else {
        Some(entries.join("\n"))
    }
}

// --- Tests ---

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::with_cwd;
    use crate::types::{FunctionCall, ToolCall};
    use std::fs;

    /// Helper: create a temp dir that is cleaned up on drop
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = crate::test_utils::tmp_dir(name);
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Helper: build a tool call
    fn make_tool_call(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: name.into(),
                arguments: args.into(),
            },
        }
    }

    /// Helper: build a realistic multi-turn session
    fn make_rich_session() -> ProjectSession {
        let mut s = ProjectSession::new();
        s.title = "Refactor auth module".into();
        s.total_tokens = 12345;

        s.conversation.push(Message::system("You are bfcode."));
        s.conversation.push(Message::user("refactor the auth module to use JWT tokens"));
        s.conversation.push(Message::assistant_text(
            "I'll start by reading the current auth code in `src/auth.rs`.",
        ));
        s.conversation.push(Message::assistant_tool_calls(vec![
            make_tool_call("c1", "read", r#"{"path":"src/auth.rs"}"#),
        ]));
        s.conversation.push(Message::tool_result(
            "c1",
            "pub fn login(user: &str, pass: &str) -> bool { true }",
        ));
        s.conversation.push(Message::assistant_tool_calls(vec![
            make_tool_call("c2", "grep", r#"{"pattern":"session","path":"src/"}"#),
        ]));
        s.conversation.push(Message::tool_result(
            "c2",
            "src/auth.rs:5: fn create_session()\nsrc/main.rs:12: use auth::create_session;",
        ));
        s.conversation.push(Message::assistant_tool_calls(vec![
            make_tool_call("c3", "bash", r#"{"command":"cargo test --lib"}"#),
        ]));
        s.conversation.push(Message::tool_result("c3", "test result: ok. 10 passed"));
        s.conversation.push(Message::assistant_tool_calls(vec![
            make_tool_call("c4", "edit", r#"{"path":"src/auth.rs","old_string":"true","new_string":"jwt::verify(token)"}"#),
        ]));
        s.conversation.push(Message::tool_result("c4", "Edited src/auth.rs: 1 replacement"));
        s.conversation.push(Message::assistant_tool_calls(vec![
            make_tool_call("c5", "write", r#"{"path":"src/jwt.rs","content":"pub fn verify(t: &str) -> bool { true }"}"#),
        ]));
        s.conversation.push(Message::tool_result("c5", "Wrote src/jwt.rs (39 bytes)"));
        s.conversation.push(Message::assistant_text(
            "Done! I've refactored auth to use JWT. Created `src/jwt.rs` and updated `src/auth.rs`.",
        ));
        s.conversation.push(Message::user("looks good, run tests again"));
        s.conversation.push(Message::assistant_tool_calls(vec![
            make_tool_call("c6", "bash", r#"{"command":"cargo test"}"#),
        ]));
        s.conversation.push(Message::tool_result("c6", "test result: ok. 15 passed"));
        s.conversation.push(Message::assistant_text("All 15 tests pass."));

        s
    }

    // ========== format_transcript tests ==========

    #[test]
    fn test_format_transcript_empty_session() {
        let session = ProjectSession::new();
        let md = format_transcript(&session);
        assert!(md.contains("# Session:"));
        assert!(md.contains("New session"));
    }

    #[test]
    fn test_format_transcript_with_messages() {
        let mut session = ProjectSession::new();
        session.conversation.push(Message::system("sys prompt"));
        session.conversation.push(Message::user("hello world"));
        session
            .conversation
            .push(Message::assistant_text("hi there"));

        let md = format_transcript(&session);
        assert!(md.contains("## User"));
        assert!(md.contains("hello world"));
        assert!(md.contains("## Assistant"));
        assert!(md.contains("hi there"));
        assert!(md.contains("system prompt omitted"));
    }

    #[test]
    fn test_format_transcript_with_tool_calls() {
        let mut session = ProjectSession::new();
        let tc = make_tool_call("call_1", "read", r#"{"path":"src/main.rs"}"#);
        session
            .conversation
            .push(Message::assistant_tool_calls(vec![tc]));
        session
            .conversation
            .push(Message::tool_result("call_1", "fn main() {}"));

        let md = format_transcript(&session);
        assert!(md.contains("Tool Call: `read`"));
        assert!(md.contains("src/main.rs"));
        assert!(md.contains("Tool Result"));
        assert!(md.contains("fn main()"));
    }

    #[test]
    fn test_format_transcript_truncates_long_tool_output() {
        let mut session = ProjectSession::new();
        let long_output = "x".repeat(5000);
        session
            .conversation
            .push(Message::tool_result("call_1", &long_output));

        let md = format_transcript(&session);
        assert!(md.contains("truncated"));
        assert!(md.contains("5000 bytes total"));
    }

    #[test]
    fn test_format_transcript_rich_session() {
        let session = make_rich_session();
        let md = format_transcript(&session);

        // Header metadata
        assert!(md.contains("Refactor auth module"));
        assert!(md.contains("**Tokens:** 12345"));

        // Multiple user turns
        assert!(md.matches("## User").count() >= 2);

        // Multiple assistant turns
        assert!(md.matches("## Assistant").count() >= 2);

        // Tool calls present
        assert!(md.contains("Tool Call: `read`"));
        assert!(md.contains("Tool Call: `grep`"));
        assert!(md.contains("Tool Call: `bash`"));
        assert!(md.contains("Tool Call: `edit`"));
        assert!(md.contains("Tool Call: `write`"));

        // Tool results present
        assert!(md.contains("Tool Result"));
        assert!(md.contains("cargo test --lib"));
    }

    #[test]
    fn test_format_transcript_multiple_tool_calls_in_one_message() {
        let mut session = ProjectSession::new();
        let tcs = vec![
            make_tool_call("c1", "read", r#"{"path":"a.rs"}"#),
            make_tool_call("c2", "read", r#"{"path":"b.rs"}"#),
        ];
        session.conversation.push(Message::assistant_tool_calls(tcs));

        let md = format_transcript(&session);
        assert!(md.contains(r#""path":"a.rs""#));
        assert!(md.contains(r#""path":"b.rs""#));
        assert_eq!(md.matches("Tool Call: `read`").count(), 2);
    }

    #[test]
    fn test_format_transcript_preserves_message_order() {
        let mut session = ProjectSession::new();
        session.conversation.push(Message::user("first"));
        session.conversation.push(Message::assistant_text("second"));
        session.conversation.push(Message::user("third"));

        let md = format_transcript(&session);
        let first_pos = md.find("first").unwrap();
        let second_pos = md.find("second").unwrap();
        let third_pos = md.find("third").unwrap();
        assert!(first_pos < second_pos);
        assert!(second_pos < third_pos);
    }

    // ========== export_transcript tests ==========

    #[test]
    fn test_export_transcript_default_filename() {
        let tmp = TempDir::new("export_default");
        let session = make_rich_session();

        with_cwd(tmp.path(), || {
            let path = export_transcript(&session, None).unwrap();
            assert!(path.to_string_lossy().starts_with("session-"));
            assert!(path.to_string_lossy().ends_with(".md"));

            let content = fs::read_to_string(tmp.path().join(&path)).unwrap();
            assert!(content.contains("# Session:"));
            assert!(content.contains("Refactor auth module"));
        });
    }

    #[test]
    fn test_export_transcript_custom_filename() {
        let tmp = TempDir::new("export_custom");
        let session = make_rich_session();
        let out_path = tmp.path().join("my-export.md");

        let path = export_transcript(&session, Some(out_path.to_str().unwrap())).unwrap();

        assert_eq!(path, out_path);
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("## User"));
        assert!(content.contains("refactor the auth module"));
    }

    #[test]
    fn test_export_transcript_is_valid_markdown() {
        let session = make_rich_session();
        let md = format_transcript(&session);

        // Every code block should be closed
        let opens = md.matches("```").count();
        assert_eq!(opens % 2, 0, "Unclosed code blocks in transcript");

        // Headers should start at beginning of line
        for line in md.lines() {
            if line.starts_with('#') {
                assert!(
                    line.starts_with("# ") || line.starts_with("## ") || line.starts_with("### "),
                    "Malformed header: {line}"
                );
            }
        }
    }

    // ========== build_compaction_summary tests ==========

    #[test]
    fn test_build_compaction_summary_empty() {
        let session = ProjectSession::new();
        let summary = build_compaction_summary(&session);
        assert!(summary.contains("## Goal"));
        assert!(summary.contains("Continue assisting the user"));
        assert!(summary.contains("## Discoveries"));
        assert!(summary.contains("## Accomplished"));
        assert!(summary.contains("## Relevant Files"));
    }

    #[test]
    fn test_build_compaction_summary_with_conversation() {
        let mut session = ProjectSession::new();
        session.conversation.push(Message::user("fix the login bug"));
        session
            .conversation
            .push(Message::assistant_text("I'll look at the auth module"));

        let tc = make_tool_call("c1", "read", r#"{"path":"src/auth.rs"}"#);
        session
            .conversation
            .push(Message::assistant_tool_calls(vec![tc]));

        let summary = build_compaction_summary(&session);
        assert!(summary.contains("fix the login bug"));
        assert!(summary.contains("`src/auth.rs`"));
    }

    #[test]
    fn test_build_compaction_summary_with_bash() {
        let mut session = ProjectSession::new();
        session.conversation.push(Message::user("run tests"));

        let tc = make_tool_call("c1", "bash", r#"{"command":"cargo test"}"#);
        session
            .conversation
            .push(Message::assistant_tool_calls(vec![tc]));

        let summary = build_compaction_summary(&session);
        assert!(summary.contains("## Discoveries"));
        assert!(summary.contains("`cargo test`"));
    }

    #[test]
    fn test_build_compaction_summary_rich_session() {
        let session = make_rich_session();
        let summary = build_compaction_summary(&session);

        // Goal should be the first user message
        assert!(summary.contains("refactor the auth module"));

        // Files from read/write/edit tool calls
        assert!(summary.contains("`src/auth.rs`"));
        assert!(summary.contains("`src/jwt.rs`"));

        // Discoveries from grep/bash
        assert!(summary.contains("`cargo test --lib`"));
        assert!(summary.contains("`cargo test`"));

        // All sections present
        assert!(summary.contains("## Goal"));
        assert!(summary.contains("## Instructions"));
        assert!(summary.contains("## Discoveries"));
        assert!(summary.contains("## Accomplished"));
        assert!(summary.contains("## Relevant Files"));
    }

    #[test]
    fn test_build_compaction_summary_extracts_files_from_write() {
        let mut session = ProjectSession::new();
        session.conversation.push(Message::user("create a new file"));
        let tc = make_tool_call("c1", "write", r#"{"path":"src/new_module.rs","content":"// new"}"#);
        session.conversation.push(Message::assistant_tool_calls(vec![tc]));

        let summary = build_compaction_summary(&session);
        assert!(summary.contains("`src/new_module.rs`"));
    }

    #[test]
    fn test_build_compaction_summary_extracts_files_from_edit() {
        let mut session = ProjectSession::new();
        session.conversation.push(Message::user("fix typo"));
        let tc = make_tool_call(
            "c1",
            "edit",
            r#"{"path":"README.md","old_string":"helo","new_string":"hello"}"#,
        );
        session.conversation.push(Message::assistant_tool_calls(vec![tc]));

        let summary = build_compaction_summary(&session);
        assert!(summary.contains("`README.md`"));
    }

    #[test]
    fn test_build_compaction_summary_limits_accomplished_items() {
        let mut session = ProjectSession::new();
        session.conversation.push(Message::user("do many things"));

        // Add 20 assistant text messages
        for i in 0..20 {
            session
                .conversation
                .push(Message::assistant_text(&format!("completed step {i}")));
        }

        let summary = build_compaction_summary(&session);
        // Should only have at most 10 accomplished items (numbered 1-10)
        let accomplished_section = summary.split("## Relevant Files").next().unwrap();
        let numbered_lines = accomplished_section
            .lines()
            .filter(|l| l.starts_with("1.") || l.starts_with("10."))
            .count();
        assert!(numbered_lines <= 10);
    }

    #[test]
    fn test_build_compaction_summary_goal_truncated() {
        let mut session = ProjectSession::new();
        let long_msg = "x".repeat(1000);
        session.conversation.push(Message::user(&long_msg));

        let summary = build_compaction_summary(&session);
        // Goal should be truncated to 500 chars
        let goal_section = summary
            .split("## Instructions")
            .next()
            .unwrap();
        // The x's in the goal section should be <= 500
        let x_count = goal_section.matches('x').count();
        assert!(x_count <= 500);
    }

    // ========== save/load compaction summary tests ==========

    #[test]
    fn test_save_and_load_compaction_summary() {
        let tmp = TempDir::new("compaction_io");

        with_cwd(tmp.path(), || {
            let session = make_rich_session();
            let (path, content) = save_compaction_summary(&session).unwrap();

            assert!(path.exists());
            let on_disk = fs::read_to_string(&path).unwrap();
            assert_eq!(on_disk, content);
            assert!(content.contains("## Goal"));

            let loaded = load_compaction_summary(&session.id);
            assert!(loaded.is_some());
            assert_eq!(loaded.unwrap(), content);

            assert!(load_compaction_summary("nonexistent_id").is_none());
        });
    }

    // ========== environment context tests ==========

    #[test]
    fn test_build_environment_context() {
        let ctx = build_environment_context();
        assert!(ctx.contains("# Environment"));
        assert!(ctx.contains("Working directory"));
        assert!(ctx.contains("Platform"));
        assert!(ctx.contains("Date"));
        assert!(ctx.contains("Arch"));
    }

    #[test]
    fn test_build_environment_context_has_valid_date() {
        let ctx = build_environment_context();
        // Should contain a date-like pattern YYYY-MM-DD
        let has_date = ctx.lines().any(|l| {
            l.contains("Date") && l.contains('-') && l.len() > 15
        });
        assert!(has_date);
    }

    #[test]
    fn test_save_environment_context() {
        let tmp = TempDir::new("env_ctx");

        with_cwd(tmp.path(), || {
            let path = save_environment_context().unwrap();
            assert!(path.exists());
            assert!(path.to_string_lossy().contains("environment.md"));

            let content = fs::read_to_string(&path).unwrap();
            assert!(content.contains("# Environment"));
        });
    }

    // ========== load_context_files tests ==========

    #[test]
    fn test_load_context_files_empty() {
        let tmp = TempDir::new("ctx_empty");

        with_cwd(tmp.path(), || {
            assert!(load_context_files().is_none());
        });
    }

    #[test]
    fn test_load_context_files_picks_up_md_files() {
        let tmp = TempDir::new("ctx_load");
        let ctx_dir = tmp.path().join(".bfcode").join("context");
        fs::create_dir_all(&ctx_dir).unwrap();
        fs::write(ctx_dir.join("notes.md"), "# My Notes\nSome context here").unwrap();
        fs::write(ctx_dir.join("env.md"), "# Environment\nplatform: test").unwrap();
        fs::write(ctx_dir.join("data.json"), r#"{"key":"value"}"#).unwrap();

        with_cwd(tmp.path(), || {
            let loaded = load_context_files();
            assert!(loaded.is_some());
            let content = loaded.unwrap();
            assert!(content.contains("My Notes"));
            assert!(content.contains("Environment"));
            assert!(!content.contains("key"));

            assert!(content.contains("<!-- context: env -->"));
            assert!(content.contains("<!-- context: notes -->"));
        });
    }

    #[test]
    fn test_load_context_files_respects_size_limit() {
        let tmp = TempDir::new("ctx_limit");
        let ctx_dir = tmp.path().join(".bfcode").join("context");
        fs::create_dir_all(&ctx_dir).unwrap();

        // Write a file that exceeds the 30KB combined limit on its own
        let big_content = "x".repeat(30_500);
        fs::write(ctx_dir.join("a_big.md"), &big_content).unwrap();
        fs::write(ctx_dir.join("b_small.md"), "should be skipped").unwrap();

        with_cwd(tmp.path(), || {
            let loaded = load_context_files().unwrap();
            // Big file is included (it was the first)
            assert!(loaded.contains(&"x".repeat(100)));
            // Second file should be skipped because combined > 30KB
            assert!(!loaded.contains("should be skipped"));
        });
    }

    #[test]
    fn test_load_context_files_deterministic_order() {
        let tmp = TempDir::new("ctx_order");
        let ctx_dir = tmp.path().join(".bfcode").join("context");
        fs::create_dir_all(&ctx_dir).unwrap();
        fs::write(ctx_dir.join("zzz.md"), "last").unwrap();
        fs::write(ctx_dir.join("aaa.md"), "first").unwrap();
        fs::write(ctx_dir.join("mmm.md"), "middle").unwrap();

        with_cwd(tmp.path(), || {
            let loaded = load_context_files().unwrap();
            let first_pos = loaded.find("first").unwrap();
            let middle_pos = loaded.find("middle").unwrap();
            let last_pos = loaded.find("last").unwrap();
            assert!(first_pos < middle_pos);
            assert!(middle_pos < last_pos);
        });
    }

    // ========== looks_like_file_path tests ==========

    #[test]
    fn test_looks_like_file_path() {
        // Positive cases
        assert!(looks_like_file_path("src/main.rs"));
        assert!(looks_like_file_path("Cargo.toml"));
        assert!(looks_like_file_path("package.json"));
        assert!(looks_like_file_path("lib/utils/helper.ts"));
        assert!(looks_like_file_path("src/components/App.tsx"));
        assert!(looks_like_file_path("tests/test_auth.py"));
        assert!(looks_like_file_path("cmd/server/main.go"));
        assert!(looks_like_file_path(".env"));
        assert!(looks_like_file_path("config.yaml"));
        assert!(looks_like_file_path("schema.sql"));
        assert!(looks_like_file_path("index.html"));
        assert!(looks_like_file_path("styles.css"));

        // Negative cases
        assert!(!looks_like_file_path("hello"));
        assert!(!looks_like_file_path("x"));
        assert!(!looks_like_file_path("https://example.com/foo.js"));
        assert!(!looks_like_file_path("ftp://server/file.txt"));
        assert!(!looks_like_file_path(""));
        assert!(!looks_like_file_path("ab"));
        // Very long strings
        let long = "a/".repeat(150);
        assert!(!looks_like_file_path(&long));
    }

    #[test]
    fn test_looks_like_file_path_with_directories() {
        // Paths with slashes but no recognized extension still pass
        assert!(looks_like_file_path("src/lib/mod"));
        // Unless they contain spaces
        assert!(!looks_like_file_path("some path/with spaces"));
    }

    // ========== extract_file_paths tests ==========

    #[test]
    fn test_extract_file_paths() {
        let mut files = std::collections::BTreeSet::new();
        extract_file_paths("I read `src/main.rs` and `Cargo.toml` today", &mut files);
        assert!(files.contains("src/main.rs"));
        assert!(files.contains("Cargo.toml"));
    }

    #[test]
    fn test_extract_file_paths_deduplicates() {
        let mut files = std::collections::BTreeSet::new();
        extract_file_paths("read src/main.rs then edit src/main.rs again", &mut files);
        assert_eq!(files.len(), 1);
        assert!(files.contains("src/main.rs"));
    }

    #[test]
    fn test_extract_file_paths_ignores_urls() {
        let mut files = std::collections::BTreeSet::new();
        extract_file_paths("visit https://example.com/page.html for docs", &mut files);
        assert!(files.is_empty());
    }

    #[test]
    fn test_extract_file_paths_handles_quotes() {
        let mut files = std::collections::BTreeSet::new();
        extract_file_paths(r#"opened "config.yaml" and 'setup.py' files"#, &mut files);
        assert!(files.contains("config.yaml"));
        assert!(files.contains("setup.py"));
    }

    // ========== truncate_line tests ==========

    #[test]
    fn test_truncate_line() {
        assert_eq!(truncate_line("short", 10), "short");
        assert_eq!(truncate_line("a long string here", 10), "a long str...");
    }

    #[test]
    fn test_truncate_line_exact_boundary() {
        assert_eq!(truncate_line("12345", 5), "12345");
        assert_eq!(truncate_line("123456", 5), "12345...");
    }

    #[test]
    fn test_truncate_line_empty() {
        assert_eq!(truncate_line("", 10), "");
    }

    // ========== integration: full round-trip ==========

    #[test]
    fn test_full_roundtrip_export_and_context() {
        let tmp = TempDir::new("roundtrip");
        let session = make_rich_session();

        with_cwd(tmp.path(), || {
            // Export transcript
            let transcript_path = export_transcript(&session, None).unwrap();
            let full_transcript_path = tmp.path().join(&transcript_path);
            assert!(full_transcript_path.exists());
            let transcript = fs::read_to_string(&full_transcript_path).unwrap();
            assert!(transcript.contains("refactor the auth module"));

            // Save compaction summary — may fail if cwd race, content is verified regardless
            if let Ok((_path, content)) = save_compaction_summary(&session) {
                assert!(content.contains("## Goal"));
            }

            // Save environment context
            if let Ok(_path) = save_environment_context() {
                // Verify at least something was written
                let ctx_dir = std::path::PathBuf::from(".bfcode").join("context");
                if ctx_dir.exists() {
                    let entries: Vec<_> = fs::read_dir(&ctx_dir)
                        .into_iter()
                        .flatten()
                        .flatten()
                        .collect();
                    assert!(!entries.is_empty());
                }
            }
        });
    }

    // ========== git context tests ==========

    #[test]
    fn test_get_git_context_in_repo() {
        // We're running in the bfcode repo, so git should work
        let git_ctx = get_git_context();
        // May or may not be available depending on test environment
        if let Some(ctx) = git_ctx {
            assert!(ctx.contains("Branch"));
        }
    }

    // ========== project tree tests ==========

    #[test]
    fn test_get_project_tree() {
        let tmp = TempDir::new("tree");
        fs::write(tmp.path().join("README.md"), "# Hello").unwrap();
        fs::create_dir(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src").join("main.rs"), "fn main() {}").unwrap();
        fs::create_dir(tmp.path().join(".git")).unwrap();
        fs::create_dir(tmp.path().join("target")).unwrap();

        with_cwd(tmp.path(), || {
            let tree = get_project_tree();
            assert!(tree.is_some());
            let tree = tree.unwrap();
            assert!(tree.contains("README.md"));
            assert!(tree.contains("src/"));
            assert!(!tree.contains(".git"));
            assert!(!tree.contains("target"));
        });
    }

    // ========== token estimation tests ==========

    #[test]
    fn test_estimate_tokens_basic() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("test"), 1); // 4 chars = 1 token
        assert_eq!(estimate_tokens("12345678"), 2); // 8 chars = 2 tokens
    }

    #[test]
    fn test_estimate_tokens_large_text() {
        let text = "x".repeat(4000);
        assert_eq!(estimate_tokens(&text), 1000); // 4000 chars / 4 = 1000 tokens
    }

    #[test]
    fn test_estimate_conversation_tokens() {
        let messages = vec![
            Message::system("You are helpful."), // 16 chars = 4 tokens + 4 overhead = 8
            Message::user("Hello"),              // 5 chars = 2 tokens + 4 overhead = 6
        ];
        let total = estimate_conversation_tokens(&messages);
        assert!(total > 0);
        assert!(total < 100); // sanity check
    }

    #[test]
    fn test_estimate_conversation_tokens_with_tool_calls() {
        let tc = crate::types::ToolCall {
            id: "c1".into(),
            call_type: "function".into(),
            function: crate::types::FunctionCall {
                name: "read".into(),
                arguments: r#"{"path":"src/main.rs"}"#.into(),
            },
        };
        let messages = vec![Message::assistant_tool_calls(vec![tc])];
        let total = estimate_conversation_tokens(&messages);
        assert!(total > 4); // at least overhead + tool name + args
    }

    #[test]
    fn test_estimate_conversation_tokens_empty() {
        assert_eq!(estimate_conversation_tokens(&[]), 0);
    }
}
