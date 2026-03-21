//! Custom agent definitions for bfcode CLI.
//!
//! Agents are defined as markdown files with YAML-style frontmatter stored in
//! `~/.bfcode/agents/` (global) and `.bfcode/agents/` (project-local).
//! Each agent defines a mode, allowed tools, system prompt, and optional model.
//!
//! ## File format
//!
//! ```markdown
//! ---
//! name: my-agent
//! description: A specialized code reviewer
//! mode: subagent
//! model: claude-sonnet-4-20250514
//! tools: read, glob, grep, list_files
//! max_rounds: 10
//! ---
//!
//! You are a specialized code reviewer. Analyze the code for...
//! ```

use anyhow::{Context, Result};
use colored::Colorize;
use std::fs;
use std::path::{Path, PathBuf};

/// A parsed agent definition loaded from a markdown file.
#[derive(Debug, Clone)]
pub struct AgentDef {
    /// Agent name (from frontmatter).
    pub name: String,
    /// Short description of the agent's purpose.
    pub description: String,
    /// Agent mode: "primary", "subagent", or "all".
    pub mode: AgentDefMode,
    /// Optional model override (e.g., "grok-4-1-fast", "ollama/llama3").
    pub model: Option<String>,
    /// Allowed tool names. Empty means use defaults for the mode.
    pub tools: Vec<String>,
    /// Maximum agentic rounds (default 15).
    pub max_rounds: usize,
    /// System prompt (markdown body after frontmatter).
    pub prompt: String,
    /// Source file path.
    pub path: PathBuf,
}

/// Agent mode variants (from markdown frontmatter).
#[derive(Debug, Clone, PartialEq)]
pub enum AgentDefMode {
    /// Can be the main conversation agent.
    Primary,
    /// Spawned via task tool, runs in child session.
    Subagent,
}

impl std::fmt::Display for AgentDefMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentDefMode::Primary => write!(f, "primary"),
            AgentDefMode::Subagent => write!(f, "subagent"),
        }
    }
}

/// Built-in agent definitions.
pub fn builtin_agents() -> Vec<AgentDef> {
    vec![
        AgentDef {
            name: "explore".into(),
            description: "Read-only file search and codebase exploration specialist".into(),
            mode: AgentDefMode::Subagent,
            model: None,
            tools: vec![
                "read", "glob", "grep", "list_files", "webfetch", "websearch",
                "memory_list", "memory_search", "pdf_read",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            max_rounds: 15,
            prompt: "You are an exploration subagent. Your job is to search the codebase, \
                     read files, and gather information. You cannot modify files. \
                     Provide a clear, detailed summary of your findings."
                .into(),
            path: PathBuf::new(),
        },
        AgentDef {
            name: "plan".into(),
            description: "Creates detailed plans without modifying code".into(),
            mode: AgentDefMode::Subagent,
            model: None,
            tools: vec![
                "read", "glob", "grep", "list_files", "webfetch", "websearch",
                "memory_list", "memory_search", "pdf_read", "write",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            max_rounds: 15,
            prompt: "You are a planning subagent. Explore the codebase and create a detailed \
                     implementation plan. You can write plans to .bfcode/plans/ but cannot \
                     modify source code. Focus on architecture, approach, and step-by-step instructions."
                .into(),
            path: PathBuf::new(),
        },
        AgentDef {
            name: "build".into(),
            description: "Full-access implementation agent".into(),
            mode: AgentDefMode::Subagent,
            model: None,
            tools: vec![
                "read", "write", "edit", "bash", "glob", "grep", "list_files",
                "apply_patch", "multiedit", "webfetch", "websearch",
                "memory_save", "memory_list", "memory_search",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            max_rounds: 15,
            prompt: "You are a build subagent. Implement the requested changes. \
                     Read files before modifying them. Prefer edit over write for existing files. \
                     Run tests after making changes."
                .into(),
            path: PathBuf::new(),
        },
    ]
}

/// Get the global agents directory (`~/.bfcode/agents/`).
pub fn agents_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    let dir = home.join(".bfcode").join("agents");
    if !dir.exists() {
        fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create agents directory: {}", dir.display()))?;
    }
    Ok(dir)
}

/// Load all agents: built-in + global + project-local.
/// Project-local agents shadow global ones, which shadow built-ins.
pub fn load_agents() -> Vec<AgentDef> {
    let mut agents = builtin_agents();

    // Global agents
    if let Ok(global_dir) = agents_dir() {
        load_agents_from_dir(&global_dir, &mut agents);
    }

    // Project-local agents
    let local_dir = PathBuf::from(".bfcode/agents");
    if local_dir.is_dir() {
        load_agents_from_dir(&local_dir, &mut agents);
    }

    agents
}

/// Load `.md` agent files from a directory, replacing existing agents with the same name.
fn load_agents_from_dir(dir: &Path, agents: &mut Vec<AgentDef>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("md") {
            if let Some(agent) = parse_agent_file(&path) {
                // Replace existing agent with same name (shadow)
                if let Some(pos) = agents.iter().position(|a| a.name == agent.name) {
                    agents[pos] = agent;
                } else {
                    agents.push(agent);
                }
            }
        }
    }
}

/// Find an agent by name (case-insensitive exact match, then partial).
pub fn find_agent<'a>(agents: &'a [AgentDef], query: &str) -> Option<&'a AgentDef> {
    let query_lower = query.to_lowercase();

    // Exact match
    if let Some(agent) = agents.iter().find(|a| a.name.to_lowercase() == query_lower) {
        return Some(agent);
    }

    // Partial match
    agents
        .iter()
        .find(|a| a.name.to_lowercase().contains(&query_lower))
}

/// Parse an agent definition from a markdown file with YAML frontmatter.
fn parse_agent_file(path: &Path) -> Option<AgentDef> {
    let raw = fs::read_to_string(path).ok()?;
    parse_agent_content(&raw, path)
}

/// Parse agent content (testable without file I/O).
pub fn parse_agent_content(raw: &str, path: &Path) -> Option<AgentDef> {
    let trimmed = raw.trim();
    if !trimmed.starts_with("---") {
        return None;
    }

    // Find closing ---
    let after_first = &trimmed[3..];
    let end_idx = after_first.find("\n---")?;
    let frontmatter = &after_first[..end_idx];
    let body_start = 3 + end_idx + 4; // "---" + content + "\n---"
    let body = if body_start < trimmed.len() {
        trimmed[body_start..].trim()
    } else {
        ""
    };

    // Parse YAML-style frontmatter (simple key: value)
    let mut name = String::new();
    let mut description = String::new();
    let mut mode_str = String::from("subagent");
    let mut model: Option<String> = None;
    let mut tools_str = String::new();
    let mut max_rounds: usize = 15;

    for line in frontmatter.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, val)) = line.split_once(':') {
            let key = key.trim();
            let val = val.trim().trim_matches('"').trim_matches('\'');
            match key {
                "name" => name = val.to_string(),
                "description" => description = val.to_string(),
                "mode" => mode_str = val.to_string(),
                "model" => {
                    if !val.is_empty() {
                        model = Some(val.to_string());
                    }
                }
                "tools" => tools_str = val.to_string(),
                "max_rounds" => {
                    if let Ok(n) = val.parse() {
                        max_rounds = n;
                    }
                }
                _ => {}
            }
        }
    }

    if name.is_empty() || description.is_empty() {
        return None;
    }

    let mode = match mode_str.as_str() {
        "primary" => AgentDefMode::Primary,
        _ => AgentDefMode::Subagent,
    };

    let tools: Vec<String> = if tools_str.is_empty() {
        Vec::new()
    } else {
        tools_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };

    let prompt = if body.is_empty() {
        description.clone()
    } else {
        body.to_string()
    };

    Some(AgentDef {
        name,
        description,
        mode,
        model,
        tools,
        max_rounds,
        prompt,
        path: path.to_path_buf(),
    })
}

/// Format the agents list for terminal display.
pub fn format_agents_list(agents: &[AgentDef]) -> String {
    if agents.is_empty() {
        return "No agents available.\n".into();
    }

    let mut output = String::new();
    for agent in agents {
        let mode_tag = format!("[{}]", agent.mode);
        let model_tag = agent
            .model
            .as_deref()
            .map(|m| format!(" model:{m}"))
            .unwrap_or_default();
        let tools_count = if agent.tools.is_empty() {
            "default tools".into()
        } else {
            format!("{} tools", agent.tools.len())
        };
        let source = if agent.path.as_os_str().is_empty() {
            "built-in".to_string()
        } else {
            agent.path.display().to_string()
        };

        output.push_str(&format!(
            "  {} {} {} — {}{} ({})\n    {}\n",
            agent.name, mode_tag, tools_count, agent.description, model_tag, source,
            agent.prompt.lines().next().unwrap_or("")
        ));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_agents_exist() {
        let agents = builtin_agents();
        assert!(agents.len() >= 3);
        let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"explore"));
        assert!(names.contains(&"plan"));
        assert!(names.contains(&"build"));
    }

    #[test]
    fn test_builtin_explore_is_readonly() {
        let agents = builtin_agents();
        let explore = agents.iter().find(|a| a.name == "explore").unwrap();
        assert_eq!(explore.mode, AgentDefMode::Subagent);
        assert!(!explore.tools.contains(&"write".to_string()));
        assert!(!explore.tools.contains(&"edit".to_string()));
        assert!(!explore.tools.contains(&"bash".to_string()));
        assert!(explore.tools.contains(&"read".to_string()));
        assert!(explore.tools.contains(&"grep".to_string()));
    }

    #[test]
    fn test_builtin_build_has_write() {
        let agents = builtin_agents();
        let build = agents.iter().find(|a| a.name == "build").unwrap();
        assert!(build.tools.contains(&"write".to_string()));
        assert!(build.tools.contains(&"edit".to_string()));
        assert!(build.tools.contains(&"bash".to_string()));
    }

    #[test]
    fn test_parse_agent_content() {
        let md = r#"---
name: reviewer
description: Code review specialist
mode: subagent
model: gpt-4o
tools: read, glob, grep
max_rounds: 10
---

You are a code reviewer. Analyze code for bugs, style issues, and potential improvements.
Provide clear, actionable feedback."#;

        let agent = parse_agent_content(md, Path::new("/tmp/reviewer.md")).unwrap();
        assert_eq!(agent.name, "reviewer");
        assert_eq!(agent.description, "Code review specialist");
        assert_eq!(agent.mode, AgentDefMode::Subagent);
        assert_eq!(agent.model, Some("gpt-4o".to_string()));
        assert_eq!(agent.tools, vec!["read", "glob", "grep"]);
        assert_eq!(agent.max_rounds, 10);
        assert!(agent.prompt.contains("code reviewer"));
    }

    #[test]
    fn test_parse_agent_minimal() {
        let md = r#"---
name: helper
description: General helper
---

Help the user."#;

        let agent = parse_agent_content(md, Path::new("/tmp/helper.md")).unwrap();
        assert_eq!(agent.name, "helper");
        assert_eq!(agent.mode, AgentDefMode::Subagent); // default
        assert_eq!(agent.model, None);
        assert!(agent.tools.is_empty());
        assert_eq!(agent.max_rounds, 15); // default
        assert!(agent.prompt.contains("Help the user"));
    }

    #[test]
    fn test_parse_agent_primary_mode() {
        let md = r#"---
name: architect
description: System architect
mode: primary
---

Design systems."#;

        let agent = parse_agent_content(md, Path::new("/tmp/arch.md")).unwrap();
        assert_eq!(agent.mode, AgentDefMode::Primary);
    }

    #[test]
    fn test_parse_agent_missing_name() {
        let md = r#"---
description: No name agent
---

Content."#;

        assert!(parse_agent_content(md, Path::new("/tmp/bad.md")).is_none());
    }

    #[test]
    fn test_parse_agent_missing_description() {
        let md = r#"---
name: noDesc
---

Content."#;

        assert!(parse_agent_content(md, Path::new("/tmp/bad.md")).is_none());
    }

    #[test]
    fn test_parse_agent_no_frontmatter() {
        let md = "Just some text without frontmatter.";
        assert!(parse_agent_content(md, Path::new("/tmp/bad.md")).is_none());
    }

    #[test]
    fn test_parse_agent_quoted_values() {
        let md = r#"---
name: "quoted-agent"
description: 'A quoted description'
tools: "read, write, edit"
---

Prompt body."#;

        let agent = parse_agent_content(md, Path::new("/tmp/quoted.md")).unwrap();
        assert_eq!(agent.name, "quoted-agent");
        assert_eq!(agent.description, "A quoted description");
        assert_eq!(agent.tools, vec!["read", "write", "edit"]);
    }

    #[test]
    fn test_find_agent_exact() {
        let agents = builtin_agents();
        let found = find_agent(&agents, "explore");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "explore");
    }

    #[test]
    fn test_find_agent_case_insensitive() {
        let agents = builtin_agents();
        let found = find_agent(&agents, "EXPLORE");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "explore");
    }

    #[test]
    fn test_find_agent_partial() {
        let agents = builtin_agents();
        let found = find_agent(&agents, "bui");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "build");
    }

    #[test]
    fn test_find_agent_not_found() {
        let agents = builtin_agents();
        assert!(find_agent(&agents, "nonexistent").is_none());
    }

    #[test]
    fn test_agent_shadowing() {
        let mut agents = builtin_agents();
        let custom = AgentDef {
            name: "explore".into(),
            description: "Custom explorer".into(),
            mode: AgentDefMode::Subagent,
            model: Some("gpt-4o".into()),
            tools: vec!["read".into()],
            max_rounds: 5,
            prompt: "Custom explore prompt".into(),
            path: PathBuf::from("/tmp/custom_explore.md"),
        };
        // Simulate shadowing
        if let Some(pos) = agents.iter().position(|a| a.name == custom.name) {
            agents[pos] = custom;
        }
        let found = find_agent(&agents, "explore").unwrap();
        assert_eq!(found.description, "Custom explorer");
        assert_eq!(found.model, Some("gpt-4o".to_string()));
        assert_eq!(found.tools, vec!["read"]);
    }

    #[test]
    fn test_format_agents_list() {
        let agents = builtin_agents();
        let output = format_agents_list(&agents);
        assert!(output.contains("explore"));
        assert!(output.contains("plan"));
        assert!(output.contains("build"));
        assert!(output.contains("[subagent]"));
    }

    #[test]
    fn test_agent_def_mode_display() {
        assert_eq!(format!("{}", AgentDefMode::Primary), "primary");
        assert_eq!(format!("{}", AgentDefMode::Subagent), "subagent");
    }
}
