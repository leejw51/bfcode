use anyhow::Result;
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::time::{Duration, timeout};

/// Hook execution points in the CLI lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookType {
    ToolBefore,
    ToolAfter,
    MessageBefore,
    MessageAfter,
    SessionStart,
    SessionEnd,
}

impl std::fmt::Display for HookType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            HookType::ToolBefore => "tool_before",
            HookType::ToolAfter => "tool_after",
            HookType::MessageBefore => "message_before",
            HookType::MessageAfter => "message_after",
            HookType::SessionStart => "session_start",
            HookType::SessionEnd => "session_end",
        };
        write!(f, "{}", s)
    }
}

/// A single hook configuration entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookConfig {
    /// When to run this hook.
    #[serde(rename = "type")]
    pub hook_type: HookType,
    /// Shell command to execute (passed to `sh -c`).
    pub command: String,
    /// Optional human-readable description.
    #[serde(default)]
    pub description: String,
    /// Whether this hook is enabled. Defaults to true.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Optional pattern to match against context (e.g., tool name for tool hooks).
    #[serde(default)]
    pub pattern: Option<String>,
}

fn default_true() -> bool {
    true
}

/// Context passed to hooks via environment variables.
#[derive(Debug, Default)]
pub struct HookContext {
    pub session_id: String,
    pub tool_name: Option<String>,
    pub tool_args: Option<String>,
    pub tool_result: Option<String>,
    pub message: Option<String>,
}

/// Intermediate struct for deserializing config files that contain a hooks array.
#[derive(Debug, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    hooks: Vec<HookConfig>,
}

/// Manages loading and execution of shell-based hooks.
///
/// Hooks are read from `.bfcode/config.json` (project-local) and
/// `~/.bfcode/config.json` (user-global). Project-local hooks are
/// loaded first, followed by global hooks.
pub struct HookManager {
    hooks: Vec<HookConfig>,
}

impl HookManager {
    /// Load hooks from `.bfcode/config.json` in the current directory
    /// and from `~/.bfcode/config.json`. Returns a manager even if no
    /// config files are found (with an empty hook list).
    pub fn load() -> Self {
        let mut hooks = Vec::new();

        // Project-local config
        let local_path = PathBuf::from(".bfcode/config.json");
        Self::load_from_file(&local_path, &mut hooks);

        // User-global config
        if let Some(home) = dirs::home_dir() {
            let global_path = home.join(".bfcode/config.json");
            Self::load_from_file(&global_path, &mut hooks);
        }

        HookManager { hooks }
    }

    /// Load hooks from a specific config file path and append them to
    /// the provided vector. Silently skips if the file doesn't exist or
    /// can't be parsed.
    fn load_from_file(path: &PathBuf, hooks: &mut Vec<HookConfig>) {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return,
        };

        let config: ConfigFile = match serde_json::from_str(&content) {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "{} Failed to parse hooks from {}: {}",
                    "warning:".yellow().bold(),
                    path.display(),
                    e
                );
                return;
            }
        };

        hooks.extend(config.hooks);
    }

    /// Run all enabled hooks matching the given type.
    ///
    /// For tool hooks (`ToolBefore` / `ToolAfter`), if a hook has a
    /// `pattern` set it is compared against `ctx.tool_name`; the hook
    /// only runs when the pattern matches.
    ///
    /// Each hook is executed via `sh -c <command>` with the following
    /// environment variables set from `ctx`:
    ///
    /// - `BFCODE_HOOK_TYPE`
    /// - `BFCODE_SESSION_ID`
    /// - `BFCODE_TOOL_NAME`
    /// - `BFCODE_TOOL_ARGS`
    /// - `BFCODE_TOOL_RESULT`
    /// - `BFCODE_MESSAGE`
    ///
    /// Each hook is given a 10-second timeout. Failures and timeouts
    /// produce a warning on stderr but never block the main flow.
    pub async fn run_hooks(&self, hook_type: HookType, ctx: &HookContext) {
        let matching: Vec<&HookConfig> = self
            .hooks
            .iter()
            .filter(|h| h.enabled && h.hook_type == hook_type)
            .filter(|h| {
                // If the hook specifies a pattern, check it against the
                // relevant context field (tool_name for tool hooks).
                if let Some(ref pattern) = h.pattern {
                    match h.hook_type {
                        HookType::ToolBefore | HookType::ToolAfter => {
                            ctx.tool_name.as_deref() == Some(pattern.as_str())
                        }
                        _ => true,
                    }
                } else {
                    true
                }
            })
            .collect();

        for hook in matching {
            self.execute_hook(hook, &hook_type, ctx).await;
        }
    }

    /// Execute a single hook command.
    async fn execute_hook(&self, hook: &HookConfig, hook_type: &HookType, ctx: &HookContext) {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(&hook.command);

        // Set environment variables from context.
        cmd.env("BFCODE_HOOK_TYPE", hook_type.to_string());
        cmd.env("BFCODE_SESSION_ID", &ctx.session_id);

        if let Some(ref name) = ctx.tool_name {
            cmd.env("BFCODE_TOOL_NAME", name);
        }
        if let Some(ref args) = ctx.tool_args {
            cmd.env("BFCODE_TOOL_ARGS", args);
        }
        if let Some(ref result) = ctx.tool_result {
            cmd.env("BFCODE_TOOL_RESULT", result);
        }
        if let Some(ref message) = ctx.message {
            cmd.env("BFCODE_MESSAGE", message);
        }

        let label = if hook.description.is_empty() {
            hook.command.clone()
        } else {
            hook.description.clone()
        };

        match timeout(Duration::from_secs(10), cmd.output()).await {
            Ok(Ok(output)) => {
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    eprintln!(
                        "{} Hook \"{}\" ({}) exited with {}: {}",
                        "warning:".yellow().bold(),
                        label,
                        hook_type,
                        output.status,
                        stderr.trim()
                    );
                }
            }
            Ok(Err(e)) => {
                eprintln!(
                    "{} Hook \"{}\" ({}) failed to execute: {}",
                    "warning:".yellow().bold(),
                    label,
                    hook_type,
                    e
                );
            }
            Err(_) => {
                eprintln!(
                    "{} Hook \"{}\" ({}) timed out after 10 seconds",
                    "warning:".yellow().bold(),
                    label,
                    hook_type,
                );
            }
        }
    }

    /// Return all configured hooks (enabled and disabled).
    pub fn list_hooks(&self) -> &[HookConfig] {
        &self.hooks
    }

    /// Returns true if at least one hook is configured.
    pub fn has_hooks(&self) -> bool {
        !self.hooks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn make_config(hooks_json: &str) -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let bfcode_dir = dir.path().join(".bfcode");
        std::fs::create_dir_all(&bfcode_dir).unwrap();
        let config_path = bfcode_dir.join("config.json");
        let mut f = std::fs::File::create(&config_path).unwrap();
        write!(f, r#"{{ "hooks": {} }}"#, hooks_json).unwrap();
        (dir, config_path)
    }

    #[test]
    fn test_hook_type_display() {
        assert_eq!(HookType::ToolBefore.to_string(), "tool_before");
        assert_eq!(HookType::SessionEnd.to_string(), "session_end");
    }

    #[test]
    fn test_deserialize_hook_config() {
        let json = r#"{
            "type": "session_start",
            "command": "echo hello",
            "description": "greet"
        }"#;
        let hook: HookConfig = serde_json::from_str(json).unwrap();
        assert_eq!(hook.hook_type, HookType::SessionStart);
        assert_eq!(hook.command, "echo hello");
        assert!(hook.enabled);
        assert!(hook.pattern.is_none());
    }

    #[test]
    fn test_load_from_file() {
        let (_dir, path) = make_config(
            r#"[
                {"type": "session_start", "command": "echo start"},
                {"type": "tool_before", "command": "echo tool", "pattern": "bash", "enabled": false}
            ]"#,
        );
        let mut hooks = Vec::new();
        HookManager::load_from_file(&path, &mut hooks);
        assert_eq!(hooks.len(), 2);
        assert_eq!(hooks[0].hook_type, HookType::SessionStart);
        assert!(!hooks[1].enabled);
        assert_eq!(hooks[1].pattern.as_deref(), Some("bash"));
    }

    #[test]
    fn test_load_missing_file() {
        let mut hooks = Vec::new();
        HookManager::load_from_file(&PathBuf::from("/nonexistent/config.json"), &mut hooks);
        assert!(hooks.is_empty());
    }

    #[test]
    fn test_has_hooks() {
        let mgr = HookManager { hooks: vec![] };
        assert!(!mgr.has_hooks());
    }

    #[tokio::test]
    async fn test_run_hooks_executes_command() {
        let hook = HookConfig {
            hook_type: HookType::SessionStart,
            command: "true".to_string(),
            description: "no-op".to_string(),
            enabled: true,
            pattern: None,
        };
        let mgr = HookManager { hooks: vec![hook] };
        let ctx = HookContext {
            session_id: "test-123".to_string(),
            ..Default::default()
        };
        // Should complete without panic.
        mgr.run_hooks(HookType::SessionStart, &ctx).await;
    }

    #[tokio::test]
    async fn test_pattern_filtering() {
        let hook = HookConfig {
            hook_type: HookType::ToolBefore,
            command: "true".to_string(),
            description: "only bash".to_string(),
            enabled: true,
            pattern: Some("bash".to_string()),
        };
        let mgr = HookManager { hooks: vec![hook] };

        // Should not match when tool_name differs.
        let ctx = HookContext {
            session_id: "s".to_string(),
            tool_name: Some("grep".to_string()),
            ..Default::default()
        };
        mgr.run_hooks(HookType::ToolBefore, &ctx).await;

        // Should match when tool_name equals pattern.
        let ctx2 = HookContext {
            session_id: "s".to_string(),
            tool_name: Some("bash".to_string()),
            ..Default::default()
        };
        mgr.run_hooks(HookType::ToolBefore, &ctx2).await;
    }

    #[tokio::test]
    async fn test_disabled_hook_skipped() {
        let hook = HookConfig {
            hook_type: HookType::SessionStart,
            command: "exit 1".to_string(),
            description: "disabled".to_string(),
            enabled: false,
            pattern: None,
        };
        let mgr = HookManager { hooks: vec![hook] };
        let ctx = HookContext::default();
        // Disabled hook should not run (exit 1 would produce a warning).
        mgr.run_hooks(HookType::SessionStart, &ctx).await;
    }
}
