use anyhow::{Context, Result, bail};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::time::{Duration, timeout};

// ===========================================================================
// Hook System
// ===========================================================================

/// Hook execution points in the CLI lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookType {
    /// Before a tool is executed.
    ToolBefore,
    /// After a tool has executed (result available).
    ToolAfter,
    /// Before a user message is processed.
    MessageBefore,
    /// After the assistant response is complete.
    MessageAfter,
    /// When a new interactive session starts.
    SessionStart,
    /// When the interactive session ends.
    SessionEnd,
    /// Before user input is sent to the LLM.
    PromptSubmit,
    /// After a complete assistant response is received.
    ResponseComplete,
    /// When an error occurs in the agent loop.
    Error,
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
            HookType::PromptSubmit => "prompt_submit",
            HookType::ResponseComplete => "response_complete",
            HookType::Error => "error",
        };
        write!(f, "{s}")
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
    /// Optional glob pattern to match against context (e.g., tool name for
    /// tool hooks). Supports `*` and `?` wildcards.
    #[serde(default)]
    pub pattern: Option<String>,
    /// Timeout in seconds for this hook. Defaults to 10.
    #[serde(default = "default_hook_timeout")]
    pub timeout: u64,
}

fn default_true() -> bool {
    true
}
fn default_hook_timeout() -> u64 {
    10
}

/// Context passed to hooks via environment variables.
#[derive(Debug, Default, Clone)]
pub struct HookContext {
    pub session_id: String,
    pub tool_name: Option<String>,
    pub tool_args: Option<String>,
    pub tool_result: Option<String>,
    pub message: Option<String>,
    pub model: Option<String>,
    pub error: Option<String>,
    pub working_dir: Option<String>,
}

/// Result of executing a hook.
#[derive(Debug, Clone)]
pub struct HookResult {
    /// Whether the hook succeeded (exit code 0).
    pub success: bool,
    /// Captured stdout from the hook (trimmed).
    pub stdout: String,
    /// Captured stderr from the hook (trimmed).
    pub stderr: String,
    /// Exit code (None if timed out or failed to execute).
    pub exit_code: Option<i32>,
}

impl HookResult {
    /// Check if the hook output indicates the action should be blocked.
    /// A hook blocks an action by outputting "BLOCK" or "DENY" to stdout.
    pub fn is_blocked(&self) -> bool {
        let s = self.stdout.trim().to_uppercase();
        s == "BLOCK" || s == "DENY"
    }
}

/// Intermediate struct for deserializing config files that contain a hooks array.
#[derive(Debug, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    hooks: Vec<HookConfig>,
    #[serde(default)]
    plugins: Vec<PluginConfig>,
}

/// Simple glob matching supporting `*` and `?` wildcards.
fn glob_match(pattern: &str, text: &str) -> bool {
    let mut p = pattern.chars().peekable();
    let mut t = text.chars().peekable();

    glob_match_inner(&mut p.collect::<Vec<_>>(), &t.collect::<Vec<_>>(), 0, 0)
}

fn glob_match_inner(pattern: &[char], text: &[char], pi: usize, ti: usize) -> bool {
    if pi == pattern.len() && ti == text.len() {
        return true;
    }
    if pi == pattern.len() {
        return false;
    }
    match pattern[pi] {
        '*' => {
            // Match zero or more characters
            for i in ti..=text.len() {
                if glob_match_inner(pattern, text, pi + 1, i) {
                    return true;
                }
            }
            false
        }
        '?' => {
            // Match exactly one character
            if ti < text.len() {
                glob_match_inner(pattern, text, pi + 1, ti + 1)
            } else {
                false
            }
        }
        c => {
            if ti < text.len() && text[ti] == c {
                glob_match_inner(pattern, text, pi + 1, ti + 1)
            } else {
                false
            }
        }
    }
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

    /// Create a HookManager with specific hooks (for testing / programmatic use).
    pub fn with_hooks(hooks: Vec<HookConfig>) -> Self {
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
    /// Returns a list of hook results. For `ToolBefore` hooks, if any
    /// result is blocked (hook outputs "BLOCK"), the caller should skip
    /// the tool execution.
    pub async fn run_hooks(&self, hook_type: HookType, ctx: &HookContext) -> Vec<HookResult> {
        let matching: Vec<&HookConfig> = self
            .hooks
            .iter()
            .filter(|h| h.enabled && h.hook_type == hook_type)
            .filter(|h| self.matches_pattern(h, ctx))
            .collect();

        let mut results = Vec::new();
        for hook in matching {
            results.push(self.execute_hook(hook, &hook_type, ctx).await);
        }
        results
    }

    /// Check if any hook result indicates blocking.
    pub fn any_blocked(results: &[HookResult]) -> bool {
        results.iter().any(|r| r.is_blocked())
    }

    /// Check if a hook's pattern matches the given context.
    fn matches_pattern(&self, hook: &HookConfig, ctx: &HookContext) -> bool {
        let Some(ref pattern) = hook.pattern else {
            return true; // no pattern = always match
        };

        match hook.hook_type {
            HookType::ToolBefore | HookType::ToolAfter => {
                if let Some(ref tool_name) = ctx.tool_name {
                    glob_match(pattern, tool_name)
                } else {
                    false
                }
            }
            _ => true,
        }
    }

    /// Execute a single hook command and capture its output.
    async fn execute_hook(
        &self,
        hook: &HookConfig,
        hook_type: &HookType,
        ctx: &HookContext,
    ) -> HookResult {
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
        if let Some(ref model) = ctx.model {
            cmd.env("BFCODE_MODEL", model);
        }
        if let Some(ref error) = ctx.error {
            cmd.env("BFCODE_ERROR", error);
        }
        if let Some(ref wd) = ctx.working_dir {
            cmd.env("BFCODE_WORKING_DIR", wd);
        }

        let label = if hook.description.is_empty() {
            hook.command.clone()
        } else {
            hook.description.clone()
        };

        let hook_timeout = Duration::from_secs(hook.timeout);

        match timeout(hook_timeout, cmd.output()).await {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let exit_code = output.status.code();
                let success = output.status.success();

                if !success {
                    eprintln!(
                        "{} Hook \"{}\" ({}) exited with {}: {}",
                        "warning:".yellow().bold(),
                        label,
                        hook_type,
                        output.status,
                        stderr
                    );
                }

                HookResult {
                    success,
                    stdout,
                    stderr,
                    exit_code,
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
                HookResult {
                    success: false,
                    stdout: String::new(),
                    stderr: e.to_string(),
                    exit_code: None,
                }
            }
            Err(_) => {
                eprintln!(
                    "{} Hook \"{}\" ({}) timed out after {} seconds",
                    "warning:".yellow().bold(),
                    label,
                    hook_type,
                    hook.timeout,
                );
                HookResult {
                    success: false,
                    stdout: String::new(),
                    stderr: "timed out".to_string(),
                    exit_code: None,
                }
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

    /// Add a hook programmatically.
    pub fn add_hook(&mut self, hook: HookConfig) {
        self.hooks.push(hook);
    }
}

// ===========================================================================
// Plugin System
// ===========================================================================

/// Plugin configuration stored in config.json under "plugins".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConfig {
    /// Unique plugin name / identifier.
    pub name: String,
    /// Path to the plugin executable or script.
    pub path: String,
    /// Whether the plugin is enabled. Defaults to true.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Plugin-specific configuration passed as JSON.
    #[serde(default)]
    pub config: serde_json::Value,
}

/// A plugin manifest (plugin.json) found in a plugin directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Plugin name.
    pub name: String,
    /// Plugin version.
    #[serde(default)]
    pub version: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// Entry point executable (relative to plugin directory).
    #[serde(default = "default_entry")]
    pub entry: String,
    /// Tools this plugin provides.
    #[serde(default)]
    pub tools: Vec<PluginToolDef>,
    /// Hooks this plugin registers.
    #[serde(default)]
    pub hooks: Vec<PluginHookDef>,
}

fn default_entry() -> String {
    "main".to_string()
}

/// A tool definition from a plugin manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginToolDef {
    /// Tool name (will be prefixed with `plugin_{plugin_name}_`).
    pub name: String,
    /// Tool description.
    #[serde(default)]
    pub description: String,
    /// JSON Schema for tool parameters.
    #[serde(default)]
    pub parameters: serde_json::Value,
}

/// A hook definition from a plugin manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginHookDef {
    /// Hook type to register for.
    #[serde(rename = "type")]
    pub hook_type: HookType,
    /// Command within the plugin to run (relative to plugin dir).
    #[serde(default)]
    pub command: String,
    /// Optional glob pattern for filtering.
    #[serde(default)]
    pub pattern: Option<String>,
}

/// A loaded plugin with its manifest and resolved paths.
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    pub manifest: PluginManifest,
    pub dir: PathBuf,
    pub enabled: bool,
}

impl LoadedPlugin {
    /// Get the full path to the plugin entry point.
    pub fn entry_path(&self) -> PathBuf {
        self.dir.join(&self.manifest.entry)
    }

    /// Convert plugin tools to bfcode ToolDefinitions.
    pub fn get_tool_definitions(&self) -> Vec<crate::types::ToolDefinition> {
        self.manifest
            .tools
            .iter()
            .map(|t| {
                let prefixed_name = format!("plugin_{}_{}", self.manifest.name, t.name);
                let params = if t.parameters.is_null() {
                    serde_json::json!({"type": "object", "properties": {}})
                } else {
                    t.parameters.clone()
                };
                crate::types::ToolDefinition {
                    tool_type: "function".into(),
                    function: crate::types::FunctionSchema {
                        name: prefixed_name,
                        description: format!("[Plugin: {}] {}", self.manifest.name, t.description),
                        parameters: params,
                    },
                }
            })
            .collect()
    }

    /// Execute a plugin tool by running the plugin entry point with the
    /// tool name and JSON arguments as command-line arguments.
    pub async fn execute_tool(&self, tool_name: &str, arguments: &str) -> Result<String> {
        let entry = self.entry_path();
        if !entry.exists() {
            bail!(
                "Plugin '{}' entry point not found: {}",
                self.manifest.name,
                entry.display()
            );
        }

        let mut cmd = tokio::process::Command::new(&entry);
        cmd.arg("tool")
            .arg(tool_name)
            .arg(arguments)
            .current_dir(&self.dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let output = timeout(Duration::from_secs(30), cmd.output())
            .await
            .context("Plugin tool execution timed out")?
            .with_context(|| {
                format!(
                    "Failed to execute plugin '{}' tool '{}'",
                    self.manifest.name, tool_name
                )
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "Plugin '{}' tool '{}' failed: {}",
                self.manifest.name,
                tool_name,
                stderr.trim()
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Convert plugin hook definitions into HookConfigs.
    pub fn to_hook_configs(&self) -> Vec<HookConfig> {
        self.manifest
            .hooks
            .iter()
            .map(|h| {
                let command = if h.command.is_empty() {
                    format!("{} hook {}", self.entry_path().display(), h.hook_type)
                } else {
                    let hook_path = self.dir.join(&h.command);
                    hook_path.display().to_string()
                };
                HookConfig {
                    hook_type: h.hook_type.clone(),
                    command,
                    description: format!("Plugin: {}", self.manifest.name),
                    enabled: self.enabled,
                    pattern: h.pattern.clone(),
                    timeout: 10,
                }
            })
            .collect()
    }
}

/// Manages loaded plugins.
pub struct PluginManager {
    plugins: Vec<LoadedPlugin>,
}

impl PluginManager {
    /// Discover and load plugins from standard directories.
    ///
    /// Plugin directories searched:
    /// 1. `.bfcode/plugins/` (project-local)
    /// 2. `~/.bfcode/plugins/` (user-global)
    ///
    /// Each subdirectory must contain a `plugin.json` manifest.
    pub fn load() -> Self {
        let mut plugins = Vec::new();

        // Project-local plugins
        let local_dir = PathBuf::from(".bfcode/plugins");
        Self::discover_plugins(&local_dir, &mut plugins);

        // User-global plugins
        if let Some(home) = dirs::home_dir() {
            let global_dir = home.join(".bfcode/plugins");
            Self::discover_plugins(&global_dir, &mut plugins);
        }

        PluginManager { plugins }
    }

    /// Create with pre-loaded plugins (for testing).
    pub fn with_plugins(plugins: Vec<LoadedPlugin>) -> Self {
        PluginManager { plugins }
    }

    /// Discover plugins in a directory. Each subdirectory with a plugin.json
    /// is treated as a plugin.
    fn discover_plugins(dir: &PathBuf, plugins: &mut Vec<LoadedPlugin>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return, // directory doesn't exist
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let manifest_path = path.join("plugin.json");
            if !manifest_path.exists() {
                continue;
            }

            match Self::load_plugin(&path, &manifest_path) {
                Ok(plugin) => {
                    eprintln!(
                        "  {} Plugin '{}' v{} ({} tools, {} hooks)",
                        "✓".green(),
                        plugin.manifest.name.cyan(),
                        plugin.manifest.version,
                        plugin.manifest.tools.len(),
                        plugin.manifest.hooks.len(),
                    );
                    plugins.push(plugin);
                }
                Err(e) => {
                    eprintln!("  {} Plugin at {}: {}", "✗".red(), path.display(), e);
                }
            }
        }
    }

    /// Load a single plugin from its directory and manifest.
    fn load_plugin(dir: &PathBuf, manifest_path: &PathBuf) -> Result<LoadedPlugin> {
        let content = std::fs::read_to_string(manifest_path)
            .with_context(|| format!("reading plugin manifest: {}", manifest_path.display()))?;

        let manifest: PluginManifest = serde_json::from_str(&content)
            .with_context(|| format!("parsing plugin manifest: {}", manifest_path.display()))?;

        Ok(LoadedPlugin {
            manifest,
            dir: dir.clone(),
            enabled: true,
        })
    }

    /// Get all tool definitions from all loaded plugins.
    pub fn get_tool_definitions(&self) -> Vec<crate::types::ToolDefinition> {
        self.plugins
            .iter()
            .filter(|p| p.enabled)
            .flat_map(|p| p.get_tool_definitions())
            .collect()
    }

    /// Get all hook configs from all loaded plugins.
    pub fn get_hook_configs(&self) -> Vec<HookConfig> {
        self.plugins
            .iter()
            .filter(|p| p.enabled)
            .flat_map(|p| p.to_hook_configs())
            .collect()
    }

    /// Execute a plugin tool. The `prefixed_name` should be like
    /// `plugin_myplug_toolname`.
    pub async fn execute_tool(&self, prefixed_name: &str, arguments: &str) -> Result<String> {
        let rest = prefixed_name
            .strip_prefix("plugin_")
            .context("Plugin tool name must start with plugin_")?;

        for plugin in &self.plugins {
            if !plugin.enabled {
                continue;
            }
            let prefix = format!("{}_", plugin.manifest.name);
            if let Some(tool_name) = rest.strip_prefix(&prefix) {
                if plugin.manifest.tools.iter().any(|t| t.name == tool_name) {
                    return plugin.execute_tool(tool_name, arguments).await;
                }
            }
        }

        bail!("No plugin found for tool: {prefixed_name}");
    }

    /// Check if a tool name is a plugin tool.
    pub fn is_plugin_tool(&self, name: &str) -> bool {
        name.starts_with("plugin_")
    }

    /// List all loaded plugins.
    pub fn list_plugins(&self) -> &[LoadedPlugin] {
        &self.plugins
    }

    /// Status report of all loaded plugins.
    pub fn status_report(&self) -> String {
        if self.plugins.is_empty() {
            return "No plugins loaded.".to_string();
        }

        let mut report = String::new();
        for p in &self.plugins {
            let status = if p.enabled { "enabled" } else { "disabled" };
            report.push_str(&format!(
                "Plugin '{}' v{} [{}]\n",
                p.manifest.name, p.manifest.version, status
            ));
            if !p.manifest.description.is_empty() {
                report.push_str(&format!("  {}\n", p.manifest.description));
            }
            for t in &p.manifest.tools {
                report.push_str(&format!("  tool: {} — {}\n", t.name, t.description));
            }
            for h in &p.manifest.hooks {
                report.push_str(&format!("  hook: {}\n", h.hook_type));
            }
        }
        report
    }
}

// Global plugin manager (set once during startup)
static PLUGIN_MANAGER: std::sync::LazyLock<tokio::sync::Mutex<Option<PluginManager>>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(None));

/// Initialize the global plugin manager.
pub async fn set_plugin_manager(manager: PluginManager) {
    let mut guard = PLUGIN_MANAGER.lock().await;
    *guard = Some(manager);
}

/// Get plugin tool definitions from the global manager.
pub async fn get_plugin_tool_definitions() -> Vec<crate::types::ToolDefinition> {
    let guard = PLUGIN_MANAGER.lock().await;
    match &*guard {
        Some(manager) => manager.get_tool_definitions(),
        None => Vec::new(),
    }
}

/// Execute a plugin tool via the global manager.
pub async fn execute_plugin_tool(name: &str, arguments: &str) -> Result<String> {
    let guard = PLUGIN_MANAGER.lock().await;
    match &*guard {
        Some(manager) => manager.execute_tool(name, arguments).await,
        None => bail!("Plugin system not initialized"),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

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

    // --- HookType tests ---

    #[test]
    fn test_hook_type_display() {
        assert_eq!(HookType::ToolBefore.to_string(), "tool_before");
        assert_eq!(HookType::ToolAfter.to_string(), "tool_after");
        assert_eq!(HookType::MessageBefore.to_string(), "message_before");
        assert_eq!(HookType::MessageAfter.to_string(), "message_after");
        assert_eq!(HookType::SessionStart.to_string(), "session_start");
        assert_eq!(HookType::SessionEnd.to_string(), "session_end");
        assert_eq!(HookType::PromptSubmit.to_string(), "prompt_submit");
        assert_eq!(HookType::ResponseComplete.to_string(), "response_complete");
        assert_eq!(HookType::Error.to_string(), "error");
    }

    #[test]
    fn test_hook_type_deserialize_all() {
        let types = vec![
            ("tool_before", HookType::ToolBefore),
            ("tool_after", HookType::ToolAfter),
            ("message_before", HookType::MessageBefore),
            ("message_after", HookType::MessageAfter),
            ("session_start", HookType::SessionStart),
            ("session_end", HookType::SessionEnd),
            ("prompt_submit", HookType::PromptSubmit),
            ("response_complete", HookType::ResponseComplete),
            ("error", HookType::Error),
        ];
        for (s, expected) in types {
            let json = format!(r#""{s}""#);
            let parsed: HookType = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, expected, "Failed for {s}");
        }
    }

    // --- HookConfig tests ---

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
        assert_eq!(hook.timeout, 10);
    }

    #[test]
    fn test_deserialize_hook_config_all_fields() {
        let json = r#"{
            "type": "tool_before",
            "command": "check_tool.sh",
            "description": "Guard bash",
            "enabled": false,
            "pattern": "bash*",
            "timeout": 5
        }"#;
        let hook: HookConfig = serde_json::from_str(json).unwrap();
        assert_eq!(hook.hook_type, HookType::ToolBefore);
        assert!(!hook.enabled);
        assert_eq!(hook.pattern.as_deref(), Some("bash*"));
        assert_eq!(hook.timeout, 5);
    }

    // --- Glob matching tests ---

    #[test]
    fn test_glob_exact_match() {
        assert!(glob_match("bash", "bash"));
        assert!(!glob_match("bash", "read"));
    }

    #[test]
    fn test_glob_star() {
        assert!(glob_match("bash*", "bash"));
        assert!(glob_match("bash*", "bash_exec"));
        assert!(glob_match("*bash", "my_bash"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("mcp_*_read", "mcp_fs_read"));
        assert!(!glob_match("mcp_*_read", "mcp_fs_write"));
    }

    #[test]
    fn test_glob_question_mark() {
        assert!(glob_match("bas?", "bash"));
        assert!(!glob_match("bas?", "ba"));
        assert!(!glob_match("bas?", "bashy"));
    }

    #[test]
    fn test_glob_combined() {
        assert!(glob_match("mcp_*_read_?ile", "mcp_fs_read_file"));
        assert!(!glob_match("mcp_*_read_?ile", "mcp_fs_read_files"));
    }

    #[test]
    fn test_glob_empty() {
        assert!(glob_match("", ""));
        assert!(!glob_match("", "x"));
        assert!(glob_match("*", ""));
    }

    // --- HookManager loading tests ---

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
        let mgr = HookManager::with_hooks(vec![]);
        assert!(!mgr.has_hooks());

        let mgr2 = HookManager::with_hooks(vec![HookConfig {
            hook_type: HookType::SessionStart,
            command: "true".into(),
            description: String::new(),
            enabled: true,
            pattern: None,
            timeout: 10,
        }]);
        assert!(mgr2.has_hooks());
    }

    #[test]
    fn test_add_hook() {
        let mut mgr = HookManager::with_hooks(vec![]);
        assert_eq!(mgr.list_hooks().len(), 0);
        mgr.add_hook(HookConfig {
            hook_type: HookType::Error,
            command: "echo error".into(),
            description: "err".into(),
            enabled: true,
            pattern: None,
            timeout: 10,
        });
        assert_eq!(mgr.list_hooks().len(), 1);
    }

    // --- Hook execution tests ---

    #[tokio::test]
    async fn test_run_hooks_executes_command() {
        let hook = HookConfig {
            hook_type: HookType::SessionStart,
            command: "true".to_string(),
            description: "no-op".to_string(),
            enabled: true,
            pattern: None,
            timeout: 10,
        };
        let mgr = HookManager::with_hooks(vec![hook]);
        let ctx = HookContext {
            session_id: "test-123".to_string(),
            ..Default::default()
        };
        let results = mgr.run_hooks(HookType::SessionStart, &ctx).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].success);
    }

    #[tokio::test]
    async fn test_hook_captures_stdout() {
        let hook = HookConfig {
            hook_type: HookType::SessionStart,
            command: "echo hello_world".to_string(),
            description: "echo".to_string(),
            enabled: true,
            pattern: None,
            timeout: 10,
        };
        let mgr = HookManager::with_hooks(vec![hook]);
        let ctx = HookContext::default();
        let results = mgr.run_hooks(HookType::SessionStart, &ctx).await;
        assert_eq!(results[0].stdout, "hello_world");
    }

    #[tokio::test]
    async fn test_hook_blocking() {
        let hook = HookConfig {
            hook_type: HookType::ToolBefore,
            command: "echo BLOCK".to_string(),
            description: "blocker".to_string(),
            enabled: true,
            pattern: None,
            timeout: 10,
        };
        let mgr = HookManager::with_hooks(vec![hook]);
        let ctx = HookContext {
            tool_name: Some("bash".into()),
            ..Default::default()
        };
        let results = mgr.run_hooks(HookType::ToolBefore, &ctx).await;
        assert!(results[0].is_blocked());
        assert!(HookManager::any_blocked(&results));
    }

    #[tokio::test]
    async fn test_hook_deny_blocking() {
        let hook = HookConfig {
            hook_type: HookType::ToolBefore,
            command: "echo DENY".to_string(),
            description: "denier".to_string(),
            enabled: true,
            pattern: None,
            timeout: 10,
        };
        let mgr = HookManager::with_hooks(vec![hook]);
        let ctx = HookContext::default();
        let results = mgr.run_hooks(HookType::ToolBefore, &ctx).await;
        assert!(results[0].is_blocked());
    }

    #[tokio::test]
    async fn test_hook_not_blocking() {
        let hook = HookConfig {
            hook_type: HookType::ToolBefore,
            command: "echo OK".to_string(),
            description: "ok".to_string(),
            enabled: true,
            pattern: None,
            timeout: 10,
        };
        let mgr = HookManager::with_hooks(vec![hook]);
        let ctx = HookContext::default();
        let results = mgr.run_hooks(HookType::ToolBefore, &ctx).await;
        assert!(!results[0].is_blocked());
        assert!(!HookManager::any_blocked(&results));
    }

    #[tokio::test]
    async fn test_glob_pattern_filtering() {
        let hook = HookConfig {
            hook_type: HookType::ToolBefore,
            command: "true".to_string(),
            description: "glob match".to_string(),
            enabled: true,
            pattern: Some("bash*".to_string()),
            timeout: 10,
        };
        let mgr = HookManager::with_hooks(vec![hook]);

        // Should NOT match "grep"
        let ctx = HookContext {
            tool_name: Some("grep".to_string()),
            ..Default::default()
        };
        let results = mgr.run_hooks(HookType::ToolBefore, &ctx).await;
        assert!(results.is_empty());

        // Should match "bash"
        let ctx2 = HookContext {
            tool_name: Some("bash".to_string()),
            ..Default::default()
        };
        let results2 = mgr.run_hooks(HookType::ToolBefore, &ctx2).await;
        assert_eq!(results2.len(), 1);

        // Should match "bash_exec"
        let ctx3 = HookContext {
            tool_name: Some("bash_exec".to_string()),
            ..Default::default()
        };
        let results3 = mgr.run_hooks(HookType::ToolBefore, &ctx3).await;
        assert_eq!(results3.len(), 1);
    }

    #[tokio::test]
    async fn test_exact_pattern_filtering() {
        let hook = HookConfig {
            hook_type: HookType::ToolBefore,
            command: "true".to_string(),
            description: "exact".to_string(),
            enabled: true,
            pattern: Some("bash".to_string()),
            timeout: 10,
        };
        let mgr = HookManager::with_hooks(vec![hook]);

        // Should match "bash" exactly
        let ctx = HookContext {
            tool_name: Some("bash".to_string()),
            ..Default::default()
        };
        let results = mgr.run_hooks(HookType::ToolBefore, &ctx).await;
        assert_eq!(results.len(), 1);

        // Should NOT match "bash_exec" (no glob)
        let ctx2 = HookContext {
            tool_name: Some("bash_exec".to_string()),
            ..Default::default()
        };
        let results2 = mgr.run_hooks(HookType::ToolBefore, &ctx2).await;
        assert!(results2.is_empty());
    }

    #[tokio::test]
    async fn test_disabled_hook_skipped() {
        let hook = HookConfig {
            hook_type: HookType::SessionStart,
            command: "exit 1".to_string(),
            description: "disabled".to_string(),
            enabled: false,
            pattern: None,
            timeout: 10,
        };
        let mgr = HookManager::with_hooks(vec![hook]);
        let ctx = HookContext::default();
        let results = mgr.run_hooks(HookType::SessionStart, &ctx).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_hook_failure_returns_result() {
        let hook = HookConfig {
            hook_type: HookType::SessionStart,
            command: "exit 42".to_string(),
            description: "fail".to_string(),
            enabled: true,
            pattern: None,
            timeout: 10,
        };
        let mgr = HookManager::with_hooks(vec![hook]);
        let ctx = HookContext::default();
        let results = mgr.run_hooks(HookType::SessionStart, &ctx).await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].success);
        assert_eq!(results[0].exit_code, Some(42));
    }

    #[tokio::test]
    async fn test_hook_env_vars() {
        let hook = HookConfig {
            hook_type: HookType::ToolAfter,
            command: "echo $BFCODE_TOOL_NAME:$BFCODE_MODEL".to_string(),
            description: "env".to_string(),
            enabled: true,
            pattern: None,
            timeout: 10,
        };
        let mgr = HookManager::with_hooks(vec![hook]);
        let ctx = HookContext {
            session_id: "s1".into(),
            tool_name: Some("bash".into()),
            model: Some("gpt-4".into()),
            ..Default::default()
        };
        let results = mgr.run_hooks(HookType::ToolAfter, &ctx).await;
        assert_eq!(results[0].stdout, "bash:gpt-4");
    }

    #[tokio::test]
    async fn test_multiple_hooks_same_type() {
        let hooks = vec![
            HookConfig {
                hook_type: HookType::SessionStart,
                command: "echo first".into(),
                description: "1".into(),
                enabled: true,
                pattern: None,
                timeout: 10,
            },
            HookConfig {
                hook_type: HookType::SessionStart,
                command: "echo second".into(),
                description: "2".into(),
                enabled: true,
                pattern: None,
                timeout: 10,
            },
            HookConfig {
                hook_type: HookType::SessionEnd,
                command: "echo end".into(),
                description: "3".into(),
                enabled: true,
                pattern: None,
                timeout: 10,
            },
        ];
        let mgr = HookManager::with_hooks(hooks);
        let ctx = HookContext::default();

        let results = mgr.run_hooks(HookType::SessionStart, &ctx).await;
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].stdout, "first");
        assert_eq!(results[1].stdout, "second");

        let end_results = mgr.run_hooks(HookType::SessionEnd, &ctx).await;
        assert_eq!(end_results.len(), 1);
    }

    #[tokio::test]
    async fn test_hook_custom_timeout() {
        let hook = HookConfig {
            hook_type: HookType::SessionStart,
            command: "sleep 5".to_string(),
            description: "slow".to_string(),
            enabled: true,
            pattern: None,
            timeout: 1, // 1 second timeout
        };
        let mgr = HookManager::with_hooks(vec![hook]);
        let ctx = HookContext::default();
        let results = mgr.run_hooks(HookType::SessionStart, &ctx).await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].success);
        assert_eq!(results[0].stderr, "timed out");
    }

    #[tokio::test]
    async fn test_new_hook_types() {
        for hook_type in [
            HookType::PromptSubmit,
            HookType::ResponseComplete,
            HookType::Error,
        ] {
            let hook = HookConfig {
                hook_type: hook_type.clone(),
                command: "echo ok".into(),
                description: format!("test {}", hook_type),
                enabled: true,
                pattern: None,
                timeout: 10,
            };
            let mgr = HookManager::with_hooks(vec![hook]);
            let ctx = HookContext::default();
            let results = mgr.run_hooks(hook_type, &ctx).await;
            assert_eq!(results.len(), 1);
            assert!(results[0].success);
        }
    }

    // --- HookResult tests ---

    #[test]
    fn test_hook_result_blocked() {
        assert!(
            HookResult {
                success: true,
                stdout: "BLOCK".into(),
                stderr: String::new(),
                exit_code: Some(0),
            }
            .is_blocked()
        );

        assert!(
            HookResult {
                success: true,
                stdout: "DENY".into(),
                stderr: String::new(),
                exit_code: Some(0),
            }
            .is_blocked()
        );

        assert!(
            HookResult {
                success: true,
                stdout: "  block  ".into(), // whitespace + case insensitive
                stderr: String::new(),
                exit_code: Some(0),
            }
            .is_blocked()
        );

        assert!(
            !HookResult {
                success: true,
                stdout: "OK".into(),
                stderr: String::new(),
                exit_code: Some(0),
            }
            .is_blocked()
        );

        assert!(
            !HookResult {
                success: true,
                stdout: "".into(),
                stderr: String::new(),
                exit_code: Some(0),
            }
            .is_blocked()
        );
    }

    // --- Plugin manifest tests ---

    #[test]
    fn test_parse_plugin_manifest() {
        let json = r#"{
            "name": "my-plugin",
            "version": "1.0.0",
            "description": "A test plugin",
            "entry": "run.sh",
            "tools": [
                {
                    "name": "greet",
                    "description": "Say hello",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "name": {"type": "string"}
                        },
                        "required": ["name"]
                    }
                }
            ],
            "hooks": [
                {
                    "type": "tool_before",
                    "command": "guard.sh",
                    "pattern": "bash*"
                }
            ]
        }"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.name, "my-plugin");
        assert_eq!(manifest.version, "1.0.0");
        assert_eq!(manifest.entry, "run.sh");
        assert_eq!(manifest.tools.len(), 1);
        assert_eq!(manifest.tools[0].name, "greet");
        assert_eq!(manifest.hooks.len(), 1);
        assert_eq!(manifest.hooks[0].hook_type, HookType::ToolBefore);
    }

    #[test]
    fn test_parse_plugin_manifest_minimal() {
        let json = r#"{"name": "tiny"}"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.name, "tiny");
        assert_eq!(manifest.entry, "main"); // default
        assert!(manifest.tools.is_empty());
        assert!(manifest.hooks.is_empty());
    }

    // --- Plugin tool definition tests ---

    #[test]
    fn test_plugin_tool_definitions() {
        let plugin = LoadedPlugin {
            manifest: PluginManifest {
                name: "myplug".into(),
                version: "1.0".into(),
                description: "test".into(),
                entry: "main".into(),
                tools: vec![
                    PluginToolDef {
                        name: "greet".into(),
                        description: "Say hi".into(),
                        parameters: serde_json::json!({
                            "type": "object",
                            "properties": {"name": {"type": "string"}}
                        }),
                    },
                    PluginToolDef {
                        name: "count".into(),
                        description: "Count things".into(),
                        parameters: serde_json::Value::Null,
                    },
                ],
                hooks: vec![],
            },
            dir: PathBuf::from("/tmp/plugins/myplug"),
            enabled: true,
        };

        let defs = plugin.get_tool_definitions();
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].function.name, "plugin_myplug_greet");
        assert!(defs[0].function.description.contains("[Plugin: myplug]"));
        assert_eq!(defs[1].function.name, "plugin_myplug_count");
        // Null params get default schema
        assert_eq!(defs[1].function.parameters["type"], "object");
    }

    // --- Plugin hook conversion tests ---

    #[test]
    fn test_plugin_hook_configs() {
        let plugin = LoadedPlugin {
            manifest: PluginManifest {
                name: "guard".into(),
                version: "1.0".into(),
                description: "".into(),
                entry: "main.sh".into(),
                tools: vec![],
                hooks: vec![
                    PluginHookDef {
                        hook_type: HookType::ToolBefore,
                        command: "check.sh".into(),
                        pattern: Some("bash*".into()),
                    },
                    PluginHookDef {
                        hook_type: HookType::SessionStart,
                        command: String::new(),
                        pattern: None,
                    },
                ],
            },
            dir: PathBuf::from("/tmp/plugins/guard"),
            enabled: true,
        };

        let hooks = plugin.to_hook_configs();
        assert_eq!(hooks.len(), 2);

        // First hook: explicit command
        assert_eq!(hooks[0].hook_type, HookType::ToolBefore);
        assert!(hooks[0].command.contains("check.sh"));
        assert_eq!(hooks[0].pattern.as_deref(), Some("bash*"));

        // Second hook: empty command uses entry point
        assert_eq!(hooks[1].hook_type, HookType::SessionStart);
        assert!(hooks[1].command.contains("main.sh"));
    }

    // --- PluginManager tests ---

    #[test]
    fn test_plugin_manager_empty() {
        let mgr = PluginManager::with_plugins(vec![]);
        assert!(mgr.list_plugins().is_empty());
        assert!(mgr.get_tool_definitions().is_empty());
        assert!(mgr.get_hook_configs().is_empty());
        assert_eq!(mgr.status_report(), "No plugins loaded.");
        assert!(!mgr.is_plugin_tool("read"));
        assert!(mgr.is_plugin_tool("plugin_x_y"));
    }

    #[test]
    fn test_plugin_discovery() {
        let dir = TempDir::new().unwrap();
        let plugins_dir = dir.path().join("plugins");
        std::fs::create_dir_all(&plugins_dir).unwrap();

        // Create a valid plugin
        let plugin_dir = plugins_dir.join("hello");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let manifest = serde_json::json!({
            "name": "hello",
            "version": "0.1.0",
            "description": "Hello plugin",
            "tools": [
                {"name": "say_hi", "description": "Says hello"}
            ]
        });
        std::fs::write(
            plugin_dir.join("plugin.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        // Create a directory without plugin.json (should be skipped)
        let no_manifest = plugins_dir.join("no-manifest");
        std::fs::create_dir_all(&no_manifest).unwrap();

        let mut plugins = Vec::new();
        PluginManager::discover_plugins(&plugins_dir, &mut plugins);

        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].manifest.name, "hello");
        assert_eq!(plugins[0].manifest.tools.len(), 1);
    }

    #[test]
    fn test_plugin_manager_tool_defs_with_disabled() {
        let plugins = vec![
            LoadedPlugin {
                manifest: PluginManifest {
                    name: "active".into(),
                    version: "1.0".into(),
                    description: "".into(),
                    entry: "main".into(),
                    tools: vec![PluginToolDef {
                        name: "tool_a".into(),
                        description: "A".into(),
                        parameters: serde_json::Value::Null,
                    }],
                    hooks: vec![],
                },
                dir: PathBuf::from("/tmp/active"),
                enabled: true,
            },
            LoadedPlugin {
                manifest: PluginManifest {
                    name: "inactive".into(),
                    version: "1.0".into(),
                    description: "".into(),
                    entry: "main".into(),
                    tools: vec![PluginToolDef {
                        name: "tool_b".into(),
                        description: "B".into(),
                        parameters: serde_json::Value::Null,
                    }],
                    hooks: vec![],
                },
                dir: PathBuf::from("/tmp/inactive"),
                enabled: false,
            },
        ];

        let mgr = PluginManager::with_plugins(plugins);
        let defs = mgr.get_tool_definitions();
        assert_eq!(defs.len(), 1); // only active plugin
        assert_eq!(defs[0].function.name, "plugin_active_tool_a");
    }

    #[test]
    fn test_plugin_status_report() {
        let plugins = vec![LoadedPlugin {
            manifest: PluginManifest {
                name: "demo".into(),
                version: "2.0".into(),
                description: "A demo plugin".into(),
                entry: "main".into(),
                tools: vec![PluginToolDef {
                    name: "demo_tool".into(),
                    description: "Does demo stuff".into(),
                    parameters: serde_json::Value::Null,
                }],
                hooks: vec![PluginHookDef {
                    hook_type: HookType::SessionStart,
                    command: "".into(),
                    pattern: None,
                }],
            },
            dir: PathBuf::from("/tmp/demo"),
            enabled: true,
        }];

        let mgr = PluginManager::with_plugins(plugins);
        let report = mgr.status_report();
        assert!(report.contains("demo"));
        assert!(report.contains("2.0"));
        assert!(report.contains("demo_tool"));
        assert!(report.contains("session_start"));
    }

    #[tokio::test]
    async fn test_plugin_execute_tool_not_found() {
        let mgr = PluginManager::with_plugins(vec![]);
        let result = mgr.execute_tool("plugin_x_y", "{}").await;
        assert!(result.is_err());
    }

    // --- Integration test: plugin with real script ---

    #[tokio::test]
    async fn test_plugin_tool_execution() {
        let dir = TempDir::new().unwrap();
        let plugin_dir = dir.path().join("test-plug");
        std::fs::create_dir_all(&plugin_dir).unwrap();

        // Create manifest
        let manifest = serde_json::json!({
            "name": "test-plug",
            "version": "1.0.0",
            "entry": "main.sh",
            "tools": [
                {"name": "echo_back", "description": "Echo arguments back"}
            ]
        });
        std::fs::write(
            plugin_dir.join("plugin.json"),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();

        // Create executable script
        let script = r#"#!/bin/sh
echo "tool=$2 args=$3"
"#;
        let script_path = plugin_dir.join("main.sh");
        std::fs::write(&script_path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let plugin = LoadedPlugin {
            manifest: serde_json::from_str(
                &std::fs::read_to_string(plugin_dir.join("plugin.json")).unwrap(),
            )
            .unwrap(),
            dir: plugin_dir,
            enabled: true,
        };

        let result = plugin
            .execute_tool("echo_back", r#"{"msg":"hi"}"#)
            .await
            .unwrap();
        assert!(result.contains("tool=echo_back"));
        assert!(result.contains(r#"args={"msg":"hi"}"#));
    }

    // --- Full lifecycle test ---

    #[tokio::test]
    async fn test_full_plugin_lifecycle() {
        let dir = TempDir::new().unwrap();
        let plugins_dir = dir.path().join("plugins");
        let plugin_dir = plugins_dir.join("counter");
        std::fs::create_dir_all(&plugin_dir).unwrap();

        // Manifest with tool and hook
        let manifest = serde_json::json!({
            "name": "counter",
            "version": "0.1.0",
            "description": "Counts things",
            "entry": "counter.sh",
            "tools": [
                {
                    "name": "count",
                    "description": "Count items",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "items": {"type": "array", "items": {"type": "string"}}
                        }
                    }
                }
            ],
            "hooks": [
                {"type": "session_start", "command": "on_start.sh"}
            ]
        });
        std::fs::write(
            plugin_dir.join("plugin.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        // Create tool script
        let tool_script = "#!/bin/sh\necho \"counted\"\n";
        let tool_path = plugin_dir.join("counter.sh");
        std::fs::write(&tool_path, tool_script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tool_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        // Create hook script
        let hook_script = "#!/bin/sh\necho \"started\"\n";
        let hook_path = plugin_dir.join("on_start.sh");
        std::fs::write(&hook_path, hook_script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        // Discover
        let mut plugins = Vec::new();
        PluginManager::discover_plugins(&plugins_dir, &mut plugins);
        assert_eq!(plugins.len(), 1);

        let mgr = PluginManager::with_plugins(plugins);

        // Check tool defs
        let defs = mgr.get_tool_definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].function.name, "plugin_counter_count");

        // Check hook configs
        let hooks = mgr.get_hook_configs();
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].hook_type, HookType::SessionStart);

        // Execute tool
        let result = mgr
            .execute_tool("plugin_counter_count", r#"{"items":["a","b"]}"#)
            .await
            .unwrap();
        assert!(result.contains("counted"));

        // Run hook via HookManager
        let hook_mgr = HookManager::with_hooks(hooks);
        let ctx = HookContext::default();
        let results = hook_mgr.run_hooks(HookType::SessionStart, &ctx).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].success);
        assert_eq!(results[0].stdout, "started");
    }
}
