use crate::types::*;
use anyhow::{Context, Result, bail, ensure};
use colored::Colorize;
use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

// Output limits (inspired by opencode)
const MAX_OUTPUT_LINES: usize = 2000;
const MAX_OUTPUT_BYTES: usize = 51200; // 50 KB
const DEFAULT_READ_LIMIT: u64 = 2000;
const MAX_CHARS_PER_LINE: usize = 2000;

pub fn get_tool_definitions() -> Vec<ToolDefinition> {
    vec![
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
    ]
}

/// Permission tracker for tool execution (like opencode's permission system)
pub struct Permissions {
    /// Tool patterns that are always allowed (e.g., "bash:cargo *", "write:*")
    always_allowed: Mutex<HashSet<String>>,
}

impl Permissions {
    pub fn new() -> Self {
        Self {
            always_allowed: Mutex::new(HashSet::new()),
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
                // Allow this tool type always for this session
                self.allow_always(&format!("{tool}:*"));
                eprintln!(
                    "  {} {}",
                    "✓".green(),
                    format!("{tool} always allowed for this session").dimmed()
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

pub async fn execute_tool(
    name: &str,
    arguments: &str,
    permissions: &Permissions,
    session_id: &str,
) -> String {
    // Check permissions for dangerous tools
    let needs_permission = matches!(name, "bash" | "write" | "edit" | "apply_patch");
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

// --- File Snapshot Helper ---

/// Save a snapshot of a file before modification (for undo support)
fn save_file_snapshot(path: &str, session_id: &str) {
    if std::path::Path::new(path).exists() {
        if let Ok(original) = std::fs::read_to_string(path) {
            let snapshot = crate::types::FileSnapshot {
                path: path.to_string(),
                original_content: original,
                timestamp: chrono::Local::now()
                    .format("%Y%m%d_%H%M%S_%3f")
                    .to_string(),
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
            tokio::fs::read_to_string(&fp.target_path).await.unwrap_or_default()
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

        tokio::fs::write(&fp.target_path, &patched).await
            .with_context(|| format!("writing {}", fp.target_path))?;

        let status = if content.is_empty() { "A" } else { "M" };
        results.push(format!("{status} {} ({} hunks applied)", fp.target_path, fp.hunks.len()));
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
                    let (old_start, old_count, new_start, new_count) = parse_hunk_header(hunk_header)?;
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
        let start_idx = if hunk.old_start == 0 { 0 } else { hunk.old_start - 1 };

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
        let old_line_count = hunk.lines.iter().filter(|l| !matches!(l, DiffLine::Add(_))).count();
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
        assert_eq!(defs.len(), 8);
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
            assert!(!def.function.description.is_empty(), "Tool {} has empty description", def.function.name);
            assert_eq!(def.tool_type, "function");
        }
    }

    #[test]
    fn test_tool_definitions_parameters_are_objects() {
        let defs = get_tool_definitions();
        for def in &defs {
            let params = &def.function.parameters;
            assert_eq!(params["type"], "object", "Tool {} params not an object", def.function.name);
            assert!(params["properties"].is_object(), "Tool {} has no properties", def.function.name);
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

        let args = format!(r#"{{"path": "{}", "offset": 5, "limit": 3}}"#, file.display());
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

        let args = format!(r#"{{"path": "{}", "content": "hello\nworld"}}"#, file.display());
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
        assert!(result.unwrap_err().to_string().contains("must be different"));

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
}
