use crate::types::{GlobalConfig, INSTRUCTION_FILES, ProjectSession};
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
        std::fs::write(&gitignore, "sessions/\ncurrent\nplans/\n")?;
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

// --- Helpers ---

fn atomic_write<T: serde::Serialize>(target: &PathBuf, data: &T) -> Result<()> {
    let json = serde_json::to_string_pretty(data)?;
    let tmp = target.with_extension("json.tmp");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, target)?;
    Ok(())
}
