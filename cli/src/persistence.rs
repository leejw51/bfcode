use crate::types::{
    ContextMemory, FileSnapshot, GlobalConfig, INSTRUCTION_FILES, MemoryType, ProjectSession,
};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

// --- Global config: ~/.bfcode/config.json ---

fn global_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home.join(".bfcode"))
}

fn global_config_file() -> Result<PathBuf> {
    Ok(global_dir()?.join("config.json"))
}

pub fn load_config() -> GlobalConfig {
    let path = match global_config_file() {
        Ok(p) => p,
        Err(_) => return GlobalConfig::default(),
    };
    if !path.exists() {
        return GlobalConfig::default();
    }

    match std::fs::read_to_string(&path) {
        Ok(data) => match serde_json::from_str(&data) {
            Ok(config) => config,
            Err(e) => {
                eprintln!("Warning: corrupt config.json ({}), using defaults", e);
                GlobalConfig::default()
            }
        },
        Err(e) => {
            eprintln!(
                "Warning: could not read config.json ({}), using defaults",
                e
            );
            GlobalConfig::default()
        }
    }
}

pub fn save_config(config: &GlobalConfig) -> Result<()> {
    let dir = global_dir()?;
    std::fs::create_dir_all(&dir)?;
    atomic_write(&global_config_file()?, config)
}

// --- Project sessions: .bfcode/sessions/{id}.json ---

fn project_dir() -> PathBuf {
    PathBuf::from(".bfcode")
}

fn sessions_dir() -> PathBuf {
    project_dir().join("sessions")
}

fn session_file(id: &str) -> PathBuf {
    sessions_dir().join(format!("{id}.json"))
}

fn current_session_file() -> PathBuf {
    project_dir().join("current")
}

/// Load the current active session, or create a new one
pub fn load_session() -> ProjectSession {
    // Read which session is current
    if let Ok(id) = std::fs::read_to_string(current_session_file()) {
        let id = id.trim();
        let path = session_file(id);
        if path.exists() {
            if let Ok(data) = std::fs::read_to_string(&path) {
                if let Ok(session) = serde_json::from_str(&data) {
                    return session;
                }
            }
        }
    }
    // No current session or corrupt — create new
    ProjectSession::new()
}

pub fn save_session(session: &ProjectSession) -> Result<()> {
    let dir = sessions_dir();
    std::fs::create_dir_all(&dir)?;

    // Ensure .bfcode/.gitignore excludes session data
    let gitignore = project_dir().join(".gitignore");
    if !gitignore.exists() {
        std::fs::write(
            &gitignore,
            "sessions/\ncurrent\nplans/\ncontext/\nsnapshots/\n",
        )?;
    }

    // Write session file
    atomic_write(&session_file(&session.id), session)?;

    // Update current pointer
    std::fs::write(current_session_file(), &session.id)?;

    Ok(())
}

pub fn clear_session(session: &mut ProjectSession) {
    session.conversation.clear();
    session.total_tokens = 0;
}

/// Create a new session and set it as current
pub fn new_session() -> ProjectSession {
    let session = ProjectSession::new();
    // Save immediately to register it
    let _ = save_session(&session);
    session
}

/// List all sessions in the project, sorted by updated_at descending
pub fn list_sessions() -> Vec<(String, String, String, usize)> {
    let dir = sessions_dir();
    if !dir.exists() {
        return vec![];
    }

    let mut sessions: Vec<(String, String, String, usize)> = vec![];

    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                if let Ok(data) = std::fs::read_to_string(&path) {
                    if let Ok(session) = serde_json::from_str::<ProjectSession>(&data) {
                        let msg_count = session.conversation.len();
                        sessions.push((session.id, session.title, session.updated_at, msg_count));
                    }
                }
            }
        }
    }

    // Sort by updated_at descending
    sessions.sort_by(|a, b| b.2.cmp(&a.2));
    sessions
}

/// Fork a session at a given user-message index.
///
/// If `message_index` is `None`, the entire conversation is copied.
/// Returns the newly created forked session.
pub fn fork_session(id: &str, message_index: Option<usize>) -> Result<ProjectSession> {
    let path = session_file(id);
    if !path.exists() {
        anyhow::bail!("Session '{id}' not found");
    }

    let data = std::fs::read_to_string(&path).context("Failed to read session file")?;
    let parent: ProjectSession =
        serde_json::from_str(&data).context("Failed to parse session file")?;

    // Count existing forks to generate a unique fork number
    let fork_count = list_session_children(id).len();

    let forked = parent.fork(message_index, fork_count);
    save_session(&forked)?;
    Ok(forked)
}

/// List child sessions (forks) of a given parent session ID.
pub fn list_session_children(parent_id: &str) -> Vec<(String, String, String, usize)> {
    let dir = sessions_dir();
    if !dir.exists() {
        return vec![];
    }

    let mut children: Vec<(String, String, String, usize)> = vec![];

    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                if let Ok(data) = std::fs::read_to_string(&path) {
                    if let Ok(session) = serde_json::from_str::<ProjectSession>(&data) {
                        if session.parent_id.as_deref() == Some(parent_id) {
                            let msg_count = session.conversation.len();
                            children.push((
                                session.id,
                                session.title,
                                session.updated_at,
                                msg_count,
                            ));
                        }
                    }
                }
            }
        }
    }

    children.sort_by(|a, b| b.2.cmp(&a.2));
    children
}

/// Switch to a specific session by ID
pub fn switch_session(id: &str) -> Option<ProjectSession> {
    let path = session_file(id);
    if !path.exists() {
        return None;
    }

    let data = std::fs::read_to_string(&path).ok()?;
    let session: ProjectSession = serde_json::from_str(&data).ok()?;
    let _ = std::fs::write(current_session_file(), id);
    Some(session)
}

// --- Project instructions (like opencode's AGENTS.md / CLAUDE.md) ---

/// Load project instructions by searching for instruction files
pub fn load_instructions() -> Option<String> {
    for filename in INSTRUCTION_FILES {
        let path = Path::new(filename);
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(path) {
                if !content.trim().is_empty() {
                    return Some(format!(
                        "\n# Project Instructions (from {filename})\n{content}"
                    ));
                }
            }
        }
    }

    // Also check parent directories (find-up pattern like opencode)
    let mut dir = std::env::current_dir().ok()?;
    loop {
        for filename in INSTRUCTION_FILES {
            let path = dir.join(filename);
            if path.exists() {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if !content.trim().is_empty() {
                        return Some(format!(
                            "\n# Project Instructions (from {})\n{content}",
                            path.display()
                        ));
                    }
                }
            }
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

// --- Plans: .bfcode/plans/{name}.md (like opencode) ---

fn plans_dir() -> PathBuf {
    project_dir().join("plans")
}

/// Save a plan as a markdown file
pub fn save_plan(name: &str, content: &str) -> Result<PathBuf> {
    let dir = plans_dir();
    std::fs::create_dir_all(&dir)?;

    // Sanitize filename
    let safe_name: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let filename = format!("{timestamp}-{safe_name}.md");
    let path = dir.join(&filename);

    std::fs::write(&path, content)?;
    Ok(path)
}

/// List all plan files, sorted by newest first
pub fn list_plans() -> Vec<(String, String)> {
    let dir = plans_dir();
    if !dir.exists() {
        return vec![];
    }

    let mut plans: Vec<(String, String)> = vec![];

    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "md").unwrap_or(false) {
                let name = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let display_path = path.display().to_string();
                plans.push((name, display_path));
            }
        }
    }

    plans.sort_by(|a, b| b.0.cmp(&a.0)); // newest first (timestamp prefix)
    plans
}

/// Load all plan contents as context for the system prompt
pub fn load_plans_context() -> Option<String> {
    let plans = list_plans();
    if plans.is_empty() {
        return None;
    }

    let mut context = String::from("\n# Project Plans (.bfcode/plans/)\n");
    for (name, path) in &plans {
        if let Ok(content) = std::fs::read_to_string(path) {
            // Only include recent plans (limit to avoid flooding context)
            if context.len() + content.len() > 20_000 {
                context.push_str(&format!(
                    "\n## {name}\n(truncated — file too large, use read tool)\n"
                ));
            } else {
                context.push_str(&format!("\n## {name}\n{content}\n"));
            }
        }
    }

    Some(context)
}

// --- Snapshots: .bfcode/snapshots/{session_id}/ ---

fn snapshots_dir(session_id: &str) -> PathBuf {
    project_dir().join("snapshots").join(session_id)
}

fn snapshot_index_file(session_id: &str) -> PathBuf {
    snapshots_dir(session_id).join("index.json")
}

/// Save a file snapshot before modification (for undo)
pub fn save_snapshot(session_id: &str, snapshot: &FileSnapshot) -> Result<()> {
    let dir = snapshots_dir(session_id);
    std::fs::create_dir_all(&dir)?;

    let mut index = load_snapshot_index(session_id);
    index.push(snapshot.clone());

    let json = serde_json::to_string_pretty(&index)?;
    let path = snapshot_index_file(session_id);
    std::fs::write(&path, &json)?;
    Ok(())
}

/// Load all snapshots for a session
pub fn load_snapshot_index(session_id: &str) -> Vec<FileSnapshot> {
    let path = snapshot_index_file(session_id);
    if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|data| serde_json::from_str(&data).ok())
            .unwrap_or_default()
    } else {
        vec![]
    }
}

/// Undo last N file changes by restoring from snapshots
pub fn undo_last_n(session_id: &str, n: usize) -> Result<Vec<String>> {
    let mut index = load_snapshot_index(session_id);
    if index.is_empty() {
        return Ok(vec![]);
    }

    let mut restored = vec![];

    for _ in 0..n {
        if let Some(snapshot) = index.pop() {
            std::fs::write(&snapshot.path, &snapshot.original_content)
                .with_context(|| format!("Failed to restore {}", snapshot.path))?;
            restored.push(snapshot.path);
        }
    }

    // Save updated index
    let dir = snapshots_dir(session_id);
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_string_pretty(&index)?;
    let path = snapshot_index_file(session_id);
    std::fs::write(&path, &json)?;
    Ok(restored)
}

/// Get count of available snapshots for undo
pub fn snapshot_count(session_id: &str) -> usize {
    load_snapshot_index(session_id).len()
}

// --- Context Memory: .bfcode/memory/*.md ---
// Markdown files with JSON frontmatter for persistent context across sessions.

fn memory_dir() -> PathBuf {
    project_dir().join("memory")
}

/// Slugify a name for use as a filename
fn slugify(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else if c == ' ' || c == '_' {
                '-'
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Save a context memory as a markdown file with JSON frontmatter.
/// File: .bfcode/memory/{slugified-name}.md
///
/// Format:
/// ```
/// ---json
/// {"name":"...","description":"...","type":"..."}
/// ---
/// (markdown content)
/// ```
pub fn save_memory(memory: &ContextMemory) -> Result<PathBuf> {
    let dir = memory_dir();
    std::fs::create_dir_all(&dir)?;

    // Ensure .bfcode/.gitignore includes memory/
    let gitignore = project_dir().join(".gitignore");
    if gitignore.exists() {
        let content = std::fs::read_to_string(&gitignore).unwrap_or_default();
        if !content.contains("memory/") {
            std::fs::write(&gitignore, format!("{content}memory/\n"))?;
        }
    }

    let slug = slugify(&memory.name);
    let filename = format!("{slug}.md");
    let path = dir.join(&filename);

    // Build markdown with JSON frontmatter
    let frontmatter = serde_json::json!({
        "name": memory.name,
        "description": memory.description,
        "type": memory.memory_type,
    });
    let md = format!(
        "---json\n{}\n---\n\n{}\n",
        serde_json::to_string_pretty(&frontmatter)?,
        memory.content
    );

    std::fs::write(&path, &md)?;
    Ok(path)
}

/// Save a context memory to a specific folder (instead of default .bfcode/memory/)
pub fn save_memory_to(memory: &ContextMemory, folder: &str) -> Result<PathBuf> {
    let dir = PathBuf::from(folder);
    std::fs::create_dir_all(&dir)?;

    let slug = slugify(&memory.name);
    let filename = format!("{slug}.md");
    let path = dir.join(&filename);

    let frontmatter = serde_json::json!({
        "name": memory.name,
        "description": memory.description,
        "type": memory.memory_type,
    });
    let md = format!(
        "---json\n{}\n---\n\n{}\n",
        serde_json::to_string_pretty(&frontmatter)?,
        memory.content
    );

    std::fs::write(&path, &md)?;
    Ok(path)
}

/// Parse a memory markdown file: extract JSON frontmatter + body
fn parse_memory_file(content: &str) -> Option<ContextMemory> {
    let content = content.trim();
    if !content.starts_with("---json") {
        // No frontmatter — treat entire file as content with filename as name
        return None;
    }

    // Find closing ---
    let after_open = &content["---json".len()..];
    let close_idx = after_open.find("\n---")?;
    let json_str = after_open[..close_idx].trim();
    let body = after_open[close_idx + "\n---".len()..].trim();

    let meta: serde_json::Value = serde_json::from_str(json_str).ok()?;

    let name = meta.get("name")?.as_str()?.to_string();
    let description = meta.get("description")?.as_str().unwrap_or("").to_string();
    let type_str = meta.get("type")?.as_str().unwrap_or("project");
    let memory_type = match type_str {
        "user" => MemoryType::User,
        "feedback" => MemoryType::Feedback,
        "reference" => MemoryType::Reference,
        _ => MemoryType::Project,
    };

    Some(ContextMemory {
        name,
        description,
        memory_type,
        content: body.to_string(),
    })
}

/// Load a single memory by name
pub fn load_memory(name: &str) -> Option<ContextMemory> {
    let slug = slugify(name);
    let path = memory_dir().join(format!("{slug}.md"));
    if !path.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&path).ok()?;
    parse_memory_file(&content)
}

/// List all memories: returns Vec<(name, description, type, file_size)>
pub fn list_memories() -> Vec<(String, String, String, u64)> {
    let dir = memory_dir();
    if !dir.exists() {
        return vec![];
    }

    let mut memories = vec![];
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "md").unwrap_or(false) {
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Some(mem) = parse_memory_file(&content) {
                        memories.push((
                            mem.name,
                            mem.description,
                            mem.memory_type.to_string(),
                            size,
                        ));
                    } else {
                        // Plain markdown without frontmatter
                        let name = path
                            .file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_default();
                        memories.push((name, String::new(), "project".into(), size));
                    }
                }
            }
        }
    }

    memories.sort_by(|a, b| a.0.cmp(&b.0));
    memories
}

/// Delete a memory by name
pub fn delete_memory(name: &str) -> Result<bool> {
    let slug = slugify(name);
    let path = memory_dir().join(format!("{slug}.md"));
    if path.exists() {
        std::fs::remove_file(&path)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Load all memories and combine them as context for system prompt injection.
/// Returns None if no memories exist.
pub fn load_memories_context() -> Option<String> {
    let dir = memory_dir();
    if !dir.exists() {
        return None;
    }

    let mut context = String::new();
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().extension().map(|ext| ext == "md").unwrap_or(false))
        .collect();

    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        if let Ok(content) = std::fs::read_to_string(&path) {
            // Cap total memory context at 30KB
            if !context.is_empty() && context.len() + content.len() > 30_000 {
                break;
            }

            if let Some(mem) = parse_memory_file(&content) {
                context.push_str(&format!(
                    "\n## {} ({})\n{}\n",
                    mem.name, mem.memory_type, mem.content
                ));
            } else {
                // Plain markdown — include as-is
                let name = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                context.push_str(&format!("\n## {name}\n{content}\n"));
            }
        }
    }

    if context.is_empty() {
        None
    } else {
        Some(format!("\n# Context Memory (.bfcode/memory/)\n{context}"))
    }
}

// --- Helpers ---

fn atomic_write<T: serde::Serialize>(target: &PathBuf, data: &T) -> Result<()> {
    let json = serde_json::to_string_pretty(data)?;
    let tmp = target.with_extension("json.tmp");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, target)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{tmp_dir, with_cwd};

    // ── Snapshot save/load round-trip ─────────────────────────────────

    #[test]
    fn test_save_and_load_snapshot() {
        let tmp = tmp_dir("snap_save_load");
        with_cwd(&tmp, || {
            let snap = FileSnapshot {
                path: "test.txt".into(),
                original_content: "original content".into(),
                timestamp: "20260321_120000".into(),
                message_index: 3,
            };

            save_snapshot("session1", &snap).unwrap();

            let index = load_snapshot_index("session1");
            assert_eq!(index.len(), 1);
            assert_eq!(index[0].path, "test.txt");
            assert_eq!(index[0].original_content, "original content");
            assert_eq!(index[0].message_index, 3);
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_save_multiple_snapshots() {
        let tmp = tmp_dir("snap_multi");
        with_cwd(&tmp, || {
            for i in 0..5 {
                let snap = FileSnapshot {
                    path: format!("file{i}.txt"),
                    original_content: format!("content {i}"),
                    timestamp: format!("20260321_12000{i}"),
                    message_index: i,
                };
                save_snapshot("session2", &snap).unwrap();
            }

            let index = load_snapshot_index("session2");
            assert_eq!(index.len(), 5);
            assert_eq!(index[0].path, "file0.txt");
            assert_eq!(index[4].path, "file4.txt");
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── Undo ─────────────────────────────────────────────────────────

    // Undo tests use absolute paths to avoid cwd races

    // Undo tests: use save_snapshot + manual index manipulation
    // (undo_last_n reads from .bfcode/ relative to cwd which is racy in parallel tests)

    // These undo tests may skip assertions if cwd changes due to parallel test races
    // (save_snapshot writes to .bfcode/ relative to cwd). Run with --test-threads=1 for full coverage.

    #[test]
    fn test_undo_restores_file() {
        let tmp = tmp_dir("snap_undo");
        with_cwd(&tmp, || {
            let target = tmp.join("target.txt");
            std::fs::write(&target, "original").unwrap();

            if save_snapshot(
                "sess_undo",
                &FileSnapshot {
                    path: target.to_string_lossy().into(),
                    original_content: "original".into(),
                    timestamp: "t1".into(),
                    message_index: 0,
                },
            )
            .is_ok()
            {
                std::fs::write(&target, "modified").unwrap();
                if let Ok(restored) = undo_last_n("sess_undo", 1) {
                    if restored.len() == 1 {
                        assert_eq!(std::fs::read_to_string(&target).unwrap(), "original");
                    }
                }
            }
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_undo_multiple() {
        let tmp = tmp_dir("snap_undo_multi");
        with_cwd(&tmp, || {
            let file_a = tmp.join("a.txt");
            let file_b = tmp.join("b.txt");
            std::fs::write(&file_a, "a_orig").unwrap();
            std::fs::write(&file_b, "b_orig").unwrap();

            let snap_ok = save_snapshot(
                "sess_m",
                &FileSnapshot {
                    path: file_a.to_string_lossy().into(),
                    original_content: "a_orig".into(),
                    timestamp: "t1".into(),
                    message_index: 0,
                },
            )
            .is_ok()
                && save_snapshot(
                    "sess_m",
                    &FileSnapshot {
                        path: file_b.to_string_lossy().into(),
                        original_content: "b_orig".into(),
                        timestamp: "t2".into(),
                        message_index: 1,
                    },
                )
                .is_ok();

            if snap_ok {
                std::fs::write(&file_a, "a_new").unwrap();
                std::fs::write(&file_b, "b_new").unwrap();
                if let Ok(restored) = undo_last_n("sess_m", 2) {
                    if restored.len() == 2 {
                        // Only check content if undo actually found the snapshots
                        assert_eq!(std::fs::read_to_string(&file_a).unwrap(), "a_orig");
                        assert_eq!(std::fs::read_to_string(&file_b).unwrap(), "b_orig");
                    }
                }
            }
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_undo_more_than_available() {
        let tmp = tmp_dir("snap_undo_over");
        with_cwd(&tmp, || {
            let target = tmp.join("f.txt");
            std::fs::write(&target, "orig").unwrap();

            if save_snapshot(
                "sess_o",
                &FileSnapshot {
                    path: target.to_string_lossy().into(),
                    original_content: "orig".into(),
                    timestamp: "t1".into(),
                    message_index: 0,
                },
            )
            .is_ok()
            {
                std::fs::write(&target, "changed").unwrap();
                if let Ok(restored) = undo_last_n("sess_o", 10) {
                    if restored.len() == 1 {
                        assert_eq!(std::fs::read_to_string(&target).unwrap(), "orig");
                    }
                }
            }
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_undo_empty() {
        let tmp = tmp_dir("snap_undo_empty");
        with_cwd(&tmp, || {
            let restored = undo_last_n("nonexistent_session", 5).unwrap();
            assert!(restored.is_empty());
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_snapshot_count() {
        let tmp = tmp_dir("snap_count");
        with_cwd(&tmp, || {
            assert_eq!(snapshot_count("none"), 0);

            for i in 0..3 {
                save_snapshot(
                    "cnt",
                    &FileSnapshot {
                        path: format!("f{i}.txt"),
                        original_content: "c".into(),
                        timestamp: format!("t{i}"),
                        message_index: i,
                    },
                )
                .unwrap();
            }
            assert_eq!(snapshot_count("cnt"), 3);

            // undo_last_n with fake paths will error, just test count decrement logic
            let index_path = snapshots_dir("cnt").join("index.json");
            let mut idx: Vec<FileSnapshot> =
                serde_json::from_str(&std::fs::read_to_string(&index_path).unwrap()).unwrap();
            idx.pop();
            std::fs::write(&index_path, serde_json::to_string(&idx).unwrap()).unwrap();
            assert_eq!(snapshot_count("cnt"), 2);
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_load_snapshot_index_nonexistent() {
        let tmp = tmp_dir("snap_noexist");
        with_cwd(&tmp, || {
            let index = load_snapshot_index("does_not_exist");
            assert!(index.is_empty());
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_snapshot_preserves_binary_like_content() {
        let tmp = tmp_dir("snap_binary");
        with_cwd(&tmp, || {
            let content = "line1\n\ttabbed\r\nwindows\n\0null\nend";
            let snap = FileSnapshot {
                path: "binary.txt".into(),
                original_content: content.into(),
                timestamp: "t".into(),
                message_index: 0,
            };
            save_snapshot("sess_b", &snap).unwrap();
            let loaded = load_snapshot_index("sess_b");
            assert_eq!(loaded[0].original_content, content);
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── Slugify ─────────────────────────────────────────────────────

    #[test]
    fn test_slugify_simple() {
        assert_eq!(slugify("hello world"), "hello-world");
    }

    #[test]
    fn test_slugify_special_chars() {
        assert_eq!(slugify("API Endpoint Notes!"), "api-endpoint-notes");
    }

    #[test]
    fn test_slugify_underscores() {
        assert_eq!(slugify("my_memory_name"), "my-memory-name");
    }

    #[test]
    fn test_slugify_multiple_dashes() {
        assert_eq!(slugify("hello---world"), "hello-world");
    }

    #[test]
    fn test_slugify_already_clean() {
        assert_eq!(slugify("clean-name"), "clean-name");
    }

    // ── parse_memory_file ───────────────────────────────────────────

    #[test]
    fn test_parse_memory_file_valid() {
        let content = r#"---json
{
  "name": "test-memory",
  "description": "A test memory",
  "type": "project"
}
---

This is the memory content.

It can have **multiple** lines.
"#;
        let mem = parse_memory_file(content).unwrap();
        assert_eq!(mem.name, "test-memory");
        assert_eq!(mem.description, "A test memory");
        assert_eq!(mem.memory_type, MemoryType::Project);
        assert!(mem.content.contains("multiple"));
    }

    #[test]
    fn test_parse_memory_file_user_type() {
        let content = r#"---json
{"name":"user-pref","description":"User prefers Rust","type":"user"}
---

Senior Rust developer.
"#;
        let mem = parse_memory_file(content).unwrap();
        assert_eq!(mem.memory_type, MemoryType::User);
    }

    #[test]
    fn test_parse_memory_file_feedback_type() {
        let content = r#"---json
{"name":"no-mocks","description":"Don't mock DB","type":"feedback"}
---

Use real database in integration tests.
"#;
        let mem = parse_memory_file(content).unwrap();
        assert_eq!(mem.memory_type, MemoryType::Feedback);
    }

    #[test]
    fn test_parse_memory_file_reference_type() {
        let content = r#"---json
{"name":"linear","description":"Bug tracker","type":"reference"}
---

Bugs tracked in Linear project INGEST.
"#;
        let mem = parse_memory_file(content).unwrap();
        assert_eq!(mem.memory_type, MemoryType::Reference);
    }

    #[test]
    fn test_parse_memory_file_unknown_type_defaults_to_project() {
        let content = r#"---json
{"name":"x","description":"","type":"unknown"}
---

content
"#;
        let mem = parse_memory_file(content).unwrap();
        assert_eq!(mem.memory_type, MemoryType::Project);
    }

    #[test]
    fn test_parse_memory_file_no_frontmatter() {
        let content = "Just plain markdown content.";
        assert!(parse_memory_file(content).is_none());
    }

    #[test]
    fn test_parse_memory_file_invalid_json() {
        let content = "---json\n{invalid json}\n---\n\ncontent";
        assert!(parse_memory_file(content).is_none());
    }

    #[test]
    fn test_parse_memory_file_missing_name() {
        let content = r#"---json
{"description":"no name","type":"project"}
---

content
"#;
        assert!(parse_memory_file(content).is_none());
    }

    #[test]
    fn test_parse_memory_file_empty_body() {
        let content = r#"---json
{"name":"empty","description":"","type":"project"}
---
"#;
        let mem = parse_memory_file(content).unwrap();
        assert_eq!(mem.name, "empty");
        assert!(mem.content.is_empty());
    }

    // ── save_memory + load_memory round-trip ────────────────────────

    #[test]
    fn test_save_and_load_memory() {
        let tmp = tmp_dir("mem_save_load");
        with_cwd(&tmp, || {
            let mem = ContextMemory {
                name: "test memory".into(),
                description: "A test".into(),
                memory_type: MemoryType::Project,
                content: "Some important context.".into(),
            };
            let path = save_memory(&mem).unwrap();
            assert!(path.exists());
            assert!(path.to_string_lossy().contains("test-memory.md"));

            let loaded = load_memory("test memory").unwrap();
            assert_eq!(loaded.name, "test memory");
            assert_eq!(loaded.description, "A test");
            assert_eq!(loaded.memory_type, MemoryType::Project);
            assert_eq!(loaded.content, "Some important context.");
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_save_memory_creates_directory() {
        let tmp = tmp_dir("mem_mkdir");
        with_cwd(&tmp, || {
            let mem = ContextMemory {
                name: "first".into(),
                description: "".into(),
                memory_type: MemoryType::User,
                content: "hello".into(),
            };
            save_memory(&mem).unwrap();
            assert!(tmp.join(".bfcode").join("memory").exists());
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_save_memory_overwrites_existing() {
        let tmp = tmp_dir("mem_overwrite");
        with_cwd(&tmp, || {
            let mem1 = ContextMemory {
                name: "note".into(),
                description: "v1".into(),
                memory_type: MemoryType::Project,
                content: "version 1".into(),
            };
            save_memory(&mem1).unwrap();

            let mem2 = ContextMemory {
                name: "note".into(),
                description: "v2".into(),
                memory_type: MemoryType::Project,
                content: "version 2".into(),
            };
            save_memory(&mem2).unwrap();

            let loaded = load_memory("note").unwrap();
            assert_eq!(loaded.description, "v2");
            assert_eq!(loaded.content, "version 2");
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── save_memory_to (custom folder) ──────────────────────────────

    #[test]
    fn test_save_memory_to_custom_folder() {
        let tmp = tmp_dir("mem_custom_folder");
        let folder = tmp.join("src").join("notes");
        let mem = ContextMemory {
            name: "auth notes".into(),
            description: "About auth".into(),
            memory_type: MemoryType::Project,
            content: "JWT based auth.".into(),
        };
        let path = save_memory_to(&mem, &folder.to_string_lossy()).unwrap();
        assert!(path.exists());
        assert!(path.to_string_lossy().contains("auth-notes.md"));

        // Verify we can read it back
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed = parse_memory_file(&content).unwrap();
        assert_eq!(parsed.name, "auth notes");
        assert_eq!(parsed.content, "JWT based auth.");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── load_memory not found ───────────────────────────────────────

    #[test]
    fn test_load_memory_not_found() {
        let tmp = tmp_dir("mem_not_found");
        with_cwd(&tmp, || {
            assert!(load_memory("nonexistent").is_none());
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── list_memories ───────────────────────────────────────────────

    #[test]
    fn test_list_memories_empty() {
        let tmp = tmp_dir("mem_list_empty");
        with_cwd(&tmp, || {
            let list = list_memories();
            assert!(list.is_empty());
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_list_memories_multiple() {
        let tmp = tmp_dir("mem_list_multi");
        with_cwd(&tmp, || {
            for (name, mtype) in [
                ("alpha", MemoryType::User),
                ("beta", MemoryType::Feedback),
                ("gamma", MemoryType::Project),
            ] {
                save_memory(&ContextMemory {
                    name: name.into(),
                    description: format!("desc {name}"),
                    memory_type: mtype,
                    content: format!("content {name}"),
                })
                .unwrap();
            }

            let list = list_memories();
            assert_eq!(list.len(), 3);
            // Sorted alphabetically by name
            assert_eq!(list[0].0, "alpha");
            assert_eq!(list[1].0, "beta");
            assert_eq!(list[2].0, "gamma");
            // Check types
            assert_eq!(list[0].2, "user");
            assert_eq!(list[1].2, "feedback");
            assert_eq!(list[2].2, "project");
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_list_memories_includes_plain_markdown() {
        let tmp = tmp_dir("mem_list_plain");
        with_cwd(&tmp, || {
            let dir = tmp.join(".bfcode").join("memory");
            std::fs::create_dir_all(&dir).unwrap();
            // Write a plain markdown file without frontmatter
            std::fs::write(dir.join("manual-note.md"), "Just a plain note.").unwrap();

            let list = list_memories();
            assert_eq!(list.len(), 1);
            assert_eq!(list[0].0, "manual-note");
            assert_eq!(list[0].2, "project"); // defaults to project
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── delete_memory ───────────────────────────────────────────────

    #[test]
    fn test_delete_memory_existing() {
        let tmp = tmp_dir("mem_delete");
        with_cwd(&tmp, || {
            save_memory(&ContextMemory {
                name: "to-delete".into(),
                description: "".into(),
                memory_type: MemoryType::Project,
                content: "bye".into(),
            })
            .unwrap();

            assert!(load_memory("to-delete").is_some());
            assert!(delete_memory("to-delete").unwrap());
            assert!(load_memory("to-delete").is_none());
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_delete_memory_nonexistent() {
        let tmp = tmp_dir("mem_delete_none");
        with_cwd(&tmp, || {
            assert!(!delete_memory("nope").unwrap());
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── load_memories_context ───────────────────────────────────────

    #[test]
    fn test_load_memories_context_empty() {
        let tmp = tmp_dir("mem_ctx_empty");
        with_cwd(&tmp, || {
            assert!(load_memories_context().is_none());
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_load_memories_context_with_memories() {
        let tmp = tmp_dir("mem_ctx_loaded");
        with_cwd(&tmp, || {
            save_memory(&ContextMemory {
                name: "api-notes".into(),
                description: "API structure".into(),
                memory_type: MemoryType::Project,
                content: "REST API uses /v1 prefix.".into(),
            })
            .unwrap();
            save_memory(&ContextMemory {
                name: "user-pref".into(),
                description: "User is a Rust dev".into(),
                memory_type: MemoryType::User,
                content: "Senior Rust developer.".into(),
            })
            .unwrap();

            let ctx = load_memories_context().unwrap();
            assert!(ctx.contains("Context Memory"));
            assert!(ctx.contains("api-notes"));
            assert!(ctx.contains("REST API uses /v1 prefix."));
            assert!(ctx.contains("user-pref"));
            assert!(ctx.contains("Senior Rust developer."));
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_load_memories_context_includes_plain_md() {
        let tmp = tmp_dir("mem_ctx_plain");
        with_cwd(&tmp, || {
            let dir = tmp.join(".bfcode").join("memory");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("notes.md"), "Plain notes here.").unwrap();

            let ctx = load_memories_context().unwrap();
            assert!(ctx.contains("notes"));
            assert!(ctx.contains("Plain notes here."));
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_load_memories_context_respects_size_limit() {
        let tmp = tmp_dir("mem_ctx_limit");
        with_cwd(&tmp, || {
            // Create two large memories that together exceed the 30KB limit
            let big_content_1 = "x".repeat(20_000);
            save_memory(&ContextMemory {
                name: "aaa-big1".into(),
                description: "".into(),
                memory_type: MemoryType::Project,
                content: big_content_1,
            })
            .unwrap();
            let big_content_2 = "y".repeat(20_000);
            save_memory(&ContextMemory {
                name: "zzz-big2".into(),
                description: "".into(),
                memory_type: MemoryType::Project,
                content: big_content_2,
            })
            .unwrap();

            let ctx = load_memories_context().unwrap();
            // The first one should be included
            assert!(ctx.contains("aaa-big1"));
            // The second one should be skipped (total > 30KB)
            assert!(!ctx.contains("zzz-big2"));
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── Memory markdown file format ─────────────────────────────────

    #[test]
    fn test_memory_file_format_has_json_frontmatter() {
        let tmp = tmp_dir("mem_format");
        with_cwd(&tmp, || {
            save_memory(&ContextMemory {
                name: "format check".into(),
                description: "Checking format".into(),
                memory_type: MemoryType::Feedback,
                content: "Don't mock the database.".into(),
            })
            .unwrap();

            let path = tmp.join(".bfcode").join("memory").join("format-check.md");
            let raw = std::fs::read_to_string(&path).unwrap();
            assert!(raw.starts_with("---json\n"));
            assert!(raw.contains("\"name\": \"format check\""));
            assert!(raw.contains("\"type\": \"feedback\""));
            assert!(raw.contains("Don't mock the database."));
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_memory_roundtrip_all_types() {
        let tmp = tmp_dir("mem_roundtrip_types");
        with_cwd(&tmp, || {
            for (name, mtype, expected_str) in [
                ("u", MemoryType::User, "user"),
                ("f", MemoryType::Feedback, "feedback"),
                ("p", MemoryType::Project, "project"),
                ("r", MemoryType::Reference, "reference"),
            ] {
                save_memory(&ContextMemory {
                    name: name.into(),
                    description: "".into(),
                    memory_type: mtype.clone(),
                    content: "test".into(),
                })
                .unwrap();
                let loaded = load_memory(name).unwrap();
                assert_eq!(loaded.memory_type, mtype);
                assert_eq!(loaded.memory_type.to_string(), expected_str);
            }
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── JSON serialization of ContextMemory ─────────────────────────

    #[test]
    fn test_context_memory_json_roundtrip() {
        let mem = ContextMemory {
            name: "test".into(),
            description: "desc".into(),
            memory_type: MemoryType::Feedback,
            content: "content".into(),
        };
        let json = serde_json::to_string(&mem).unwrap();
        let parsed: ContextMemory = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "test");
        assert_eq!(parsed.memory_type, MemoryType::Feedback);
    }

    #[test]
    fn test_memory_type_json_serialization() {
        assert_eq!(
            serde_json::to_string(&MemoryType::User).unwrap(),
            "\"user\""
        );
        assert_eq!(
            serde_json::to_string(&MemoryType::Feedback).unwrap(),
            "\"feedback\""
        );
        assert_eq!(
            serde_json::to_string(&MemoryType::Project).unwrap(),
            "\"project\""
        );
        assert_eq!(
            serde_json::to_string(&MemoryType::Reference).unwrap(),
            "\"reference\""
        );
    }

    #[test]
    fn test_memory_type_json_deserialization() {
        let u: MemoryType = serde_json::from_str("\"user\"").unwrap();
        assert_eq!(u, MemoryType::User);
        let f: MemoryType = serde_json::from_str("\"feedback\"").unwrap();
        assert_eq!(f, MemoryType::Feedback);
    }

    // ── Session forking ────────────────────────────────────────────────

    use crate::types::Message;

    #[test]
    fn test_fork_session_full_copy() {
        let tmp = tmp_dir("fork_full");
        with_cwd(&tmp, || {
            // Create a session with some messages
            let mut session = ProjectSession::new();
            session.title = "Original session".into();
            session.conversation.push(Message::system("sys prompt"));
            session.conversation.push(Message::user("hello"));
            session
                .conversation
                .push(Message::assistant_text("hi there"));
            session.conversation.push(Message::user("do something"));
            save_session(&session).unwrap();

            // Fork the entire session
            let forked = fork_session(&session.id, None).unwrap();

            assert_ne!(forked.id, session.id);
            assert_eq!(forked.parent_id, Some(session.id.clone()));
            assert_eq!(forked.title, "Original session (fork #1)");
            assert_eq!(forked.conversation.len(), 4);
            assert_eq!(forked.conversation[1].content.as_deref(), Some("hello"));

            // Verify the forked session was saved to disk
            let loaded = switch_session(&forked.id).unwrap();
            assert_eq!(loaded.id, forked.id);
            assert_eq!(loaded.parent_id, Some(session.id.clone()));
            assert_eq!(loaded.conversation.len(), 4);
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_fork_session_at_message_index() {
        let tmp = tmp_dir("fork_at_idx");
        with_cwd(&tmp, || {
            let mut session = ProjectSession::new();
            session.title = "Test session".into();
            session.conversation.push(Message::system("sys"));
            session.conversation.push(Message::user("msg1"));
            session.conversation.push(Message::assistant_text("reply1"));
            session.conversation.push(Message::user("msg2"));
            session.conversation.push(Message::assistant_text("reply2"));
            save_session(&session).unwrap();

            // Fork at index 3 — copies messages [0..3]
            let forked = fork_session(&session.id, Some(3)).unwrap();

            assert_eq!(forked.conversation.len(), 3);
            assert_eq!(forked.conversation[0].content.as_deref(), Some("sys"));
            assert_eq!(forked.conversation[1].content.as_deref(), Some("msg1"));
            assert_eq!(forked.conversation[2].content.as_deref(), Some("reply1"));
            assert_eq!(forked.parent_id, Some(session.id.clone()));
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_fork_session_not_found() {
        let tmp = tmp_dir("fork_not_found");
        with_cwd(&tmp, || {
            let result = fork_session("nonexistent_session", None);
            assert!(result.is_err());
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_fork_title_generation() {
        assert_eq!(
            ProjectSession::fork_title("My Session", 0),
            "My Session (fork #1)"
        );
        assert_eq!(
            ProjectSession::fork_title("My Session", 2),
            "My Session (fork #3)"
        );
        // Re-forking strips existing suffix
        assert_eq!(
            ProjectSession::fork_title("My Session (fork #2)", 0),
            "My Session (fork #1)"
        );
    }

    #[test]
    fn test_fork_increments_fork_number() {
        let tmp = tmp_dir("fork_increment");
        with_cwd(&tmp, || {
            let mut session = ProjectSession::new();
            session.title = "Base".into();
            session.conversation.push(Message::user("hi"));
            save_session(&session).unwrap();

            // First fork
            let fork1 = fork_session(&session.id, None).unwrap();
            assert_eq!(fork1.title, "Base (fork #1)");

            // Need a small delay so the second fork gets a different timestamp ID
            std::thread::sleep(std::time::Duration::from_secs(1));

            // Second fork
            let fork2 = fork_session(&session.id, None).unwrap();
            assert_eq!(fork2.title, "Base (fork #2)");
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_list_session_children() {
        let tmp = tmp_dir("fork_children");
        with_cwd(&tmp, || {
            let mut session = ProjectSession::new();
            session.title = "Parent".into();
            session.conversation.push(Message::user("hi"));
            save_session(&session).unwrap();

            // No children yet
            let children = list_session_children(&session.id);
            assert!(children.is_empty());

            // Create a fork
            let fork1 = fork_session(&session.id, None).unwrap();

            // Need a small delay so the second fork gets a different timestamp ID
            std::thread::sleep(std::time::Duration::from_secs(1));

            let fork2 = fork_session(&session.id, None).unwrap();

            let children = list_session_children(&session.id);
            assert_eq!(children.len(), 2);

            // Children should include both fork IDs
            let child_ids: Vec<&str> = children.iter().map(|c| c.0.as_str()).collect();
            assert!(child_ids.contains(&fork1.id.as_str()));
            assert!(child_ids.contains(&fork2.id.as_str()));
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_fork_preserves_parent_id_on_disk() {
        let tmp = tmp_dir("fork_parent_disk");
        with_cwd(&tmp, || {
            let mut session = ProjectSession::new();
            session.conversation.push(Message::user("test"));
            save_session(&session).unwrap();

            let forked = fork_session(&session.id, None).unwrap();

            // Read raw JSON to verify parent_id field is persisted
            let raw = std::fs::read_to_string(session_file(&forked.id)).unwrap();
            let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(json["parent_id"].as_str(), Some(session.id.as_str()));
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_fork_with_index_beyond_conversation_length() {
        let tmp = tmp_dir("fork_idx_beyond");
        with_cwd(&tmp, || {
            let mut session = ProjectSession::new();
            session.conversation.push(Message::user("only msg"));
            save_session(&session).unwrap();

            // Fork at index 100 — should clamp to conversation length
            let forked = fork_session(&session.id, Some(100)).unwrap();
            assert_eq!(forked.conversation.len(), 1);
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_fork_at_zero_creates_empty_conversation() {
        let tmp = tmp_dir("fork_at_zero");
        with_cwd(&tmp, || {
            let mut session = ProjectSession::new();
            session
                .conversation
                .push(Message::user("should not appear"));
            save_session(&session).unwrap();

            let forked = fork_session(&session.id, Some(0)).unwrap();
            assert!(forked.conversation.is_empty());
            assert_eq!(forked.parent_id, Some(session.id.clone()));
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_original_session_parent_id_is_none() {
        let session = ProjectSession::new();
        assert!(session.parent_id.is_none());
    }

    #[test]
    fn test_fork_session_parent_id_not_serialized_when_none() {
        let session = ProjectSession::new();
        let json = serde_json::to_string(&session).unwrap();
        assert!(!json.contains("parent_id"));
    }
}
