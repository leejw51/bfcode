use anyhow::{bail, Context, Result};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// The full merged configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FullConfig {
    /// Model identifier (e.g. "claude-opus-4-6").
    pub model: String,
    /// Sampling temperature (0.0 – 2.0).
    pub temperature: f64,
    /// Provider name (e.g. "anthropic", "openai").
    pub provider: String,

    /// Optional gateway settings.
    #[serde(default)]
    pub gateway: Option<GatewaySection>,

    /// Optional daemon settings.
    #[serde(default)]
    pub daemon: Option<DaemonSection>,

    /// Lifecycle hooks (arbitrary JSON values).
    #[serde(default)]
    pub hooks: Vec<serde_json::Value>,

    /// Custom environment variables to inject.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Paths to other config files to include/merge.
    #[serde(default)]
    pub include: Vec<String>,

    /// Schema version used for automatic migration.
    #[serde(default = "default_version")]
    pub config_version: u32,
}

fn default_version() -> u32 {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GatewaySection {
    #[serde(default)]
    pub listen: Option<String>,
    #[serde(default)]
    pub api_keys: Vec<String>,
    #[serde(default)]
    pub tailscale: bool,
    #[serde(default)]
    pub max_sessions: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DaemonSection {
    #[serde(default)]
    pub respawn: Option<bool>,
    #[serde(default)]
    pub max_respawns: Option<u32>,
    #[serde(default)]
    pub auto_update_hours: Option<u32>,
    #[serde(default)]
    pub log_file: Option<String>,
}

/// A single validation error produced by [`validate_config`].
#[derive(Debug, Clone)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

/// Metadata about a discovered config file.
#[derive(Debug, Clone)]
pub struct ConfigSource {
    pub path: PathBuf,
    pub format: ConfigFormat,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConfigFormat {
    Json,
    Yaml,
}

// ---------------------------------------------------------------------------
// Default config
// ---------------------------------------------------------------------------

fn default_config_value() -> serde_json::Value {
    serde_json::json!({
        "model": "claude-opus-4-6",
        "temperature": 1.0,
        "provider": "anthropic",
        "hooks": [],
        "env": {},
        "include": [],
        "config_version": 2
    })
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Load and merge configuration from all sources.
///
/// Priority (highest wins): env vars > project config > global config > defaults.
pub fn load_full_config() -> Result<FullConfig> {
    let sources = find_config_files();

    // Start with built-in defaults.
    let mut merged = default_config_value();

    for source in &sources {
        let mut val = load_config_file(&source.path)
            .with_context(|| format!("loading config from {}", source.path.display()))?;

        // Resolve include directives relative to the config file's directory.
        if let Some(dir) = source.path.parent() {
            process_includes(&mut val, dir)?;
        }

        merged = merge_configs(merged, val);
    }

    // Migrate old config versions forward.
    let migrated = migrate_config(&mut merged)?;
    if migrated {
        eprintln!(
            "{} config was auto-migrated to version 2",
            "note:".yellow().bold()
        );
    }

    // Apply environment-variable overrides last (highest priority).
    apply_env_overrides(&mut merged);

    // Validate and warn (non-fatal).
    let errors = validate_config(&merged);
    for err in &errors {
        eprintln!("{} {}", "config warning:".yellow().bold(), err);
    }

    let config: FullConfig = serde_json::from_value(merged)
        .context("deserializing merged config into FullConfig")?;

    Ok(config)
}

/// Load a single config file.  Dispatches to JSON or YAML based on extension.
pub fn load_config_file(path: &Path) -> Result<serde_json::Value> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;

    match detect_format(path) {
        ConfigFormat::Json => {
            serde_json::from_str(&content).with_context(|| format!("parsing JSON {}", path.display()))
        }
        ConfigFormat::Yaml => parse_simple_yaml(&content),
    }
}

/// Parse a *simple* YAML subset into a [`serde_json::Value`].
///
/// Supported:
/// - `key: value` (string / number / bool)
/// - `key:` + indented block (nested object)
/// - `- item` under a key (array)
/// - `#` comments
/// - Quoted strings (`"..."` / `'...'`)
///
/// This is intentionally NOT a full YAML parser.
pub fn parse_simple_yaml(content: &str) -> Result<serde_json::Value> {
    let lines: Vec<&str> = content.lines().collect();
    let (val, _) = parse_yaml_block(&lines, 0, 0)?;
    Ok(val)
}

/// Deep-merge two JSON values.  Objects are merged recursively, arrays are
/// concatenated, and scalars are overwritten by `overlay`.
pub fn merge_configs(
    base: serde_json::Value,
    overlay: serde_json::Value,
) -> serde_json::Value {
    use serde_json::Value;

    match (base, overlay) {
        (Value::Object(mut base_map), Value::Object(overlay_map)) => {
            for (k, v) in overlay_map {
                let merged = if let Some(existing) = base_map.remove(&k) {
                    merge_configs(existing, v)
                } else {
                    v
                };
                base_map.insert(k, merged);
            }
            Value::Object(base_map)
        }
        (Value::Array(mut base_arr), Value::Array(overlay_arr)) => {
            base_arr.extend(overlay_arr);
            Value::Array(base_arr)
        }
        // Scalar or type mismatch – overlay wins.
        (_base, overlay) => overlay,
    }
}

/// Resolve `"include"` directives inside a config value.
///
/// Each entry in the `include` array is a path (relative to `base_dir`).  The
/// referenced files are loaded, merged into the current value, and the
/// `include` key is removed afterwards.
pub fn process_includes(config: &mut serde_json::Value, base_dir: &Path) -> Result<()> {
    let includes: Vec<String> = match config.get("include") {
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => return Ok(()),
    };

    // Remove the include key so it doesn't cascade or duplicate.
    if let Some(obj) = config.as_object_mut() {
        obj.remove("include");
    }

    for inc_path_str in &includes {
        let inc_path = base_dir.join(inc_path_str);
        if !inc_path.exists() {
            eprintln!(
                "{} included config not found: {}",
                "warning:".yellow().bold(),
                inc_path.display()
            );
            continue;
        }

        let mut inc_val = load_config_file(&inc_path)
            .with_context(|| format!("loading included config {}", inc_path.display()))?;

        // Recurse so included files can themselves include others.
        if let Some(dir) = inc_path.parent() {
            process_includes(&mut inc_val, dir)?;
        }

        // Merge: the including file's own values take precedence over the
        // included file's values (included acts as a base).
        *config = merge_configs(inc_val, config.clone());
    }

    Ok(())
}

/// Validate a config JSON value.  Returns a (possibly empty) list of problems.
pub fn validate_config(config: &serde_json::Value) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    // model – non-empty string
    match config.get("model") {
        Some(serde_json::Value::String(s)) if s.is_empty() => {
            errors.push(ValidationError {
                field: "model".into(),
                message: "must be a non-empty string".into(),
            });
        }
        Some(serde_json::Value::String(_)) => {}
        Some(_) => {
            errors.push(ValidationError {
                field: "model".into(),
                message: "must be a string".into(),
            });
        }
        None => {} // optional at validation time (defaults fill it)
    }

    // temperature – 0.0..=2.0
    if let Some(t) = config.get("temperature") {
        if let Some(n) = t.as_f64() {
            if !(0.0..=2.0).contains(&n) {
                errors.push(ValidationError {
                    field: "temperature".into(),
                    message: format!("must be between 0.0 and 2.0, got {n}"),
                });
            }
        } else {
            errors.push(ValidationError {
                field: "temperature".into(),
                message: "must be a number".into(),
            });
        }
    }

    // gateway.listen – host:port
    if let Some(gw) = config.get("gateway") {
        if let Some(listen) = gw.get("listen") {
            if let Some(s) = listen.as_str() {
                if !s.contains(':') {
                    errors.push(ValidationError {
                        field: "gateway.listen".into(),
                        message: format!("must be in host:port format, got \"{s}\""),
                    });
                } else {
                    let parts: Vec<&str> = s.rsplitn(2, ':').collect();
                    if parts[0].parse::<u16>().is_err() {
                        errors.push(ValidationError {
                            field: "gateway.listen".into(),
                            message: format!(
                                "port part must be a valid u16, got \"{}\"",
                                parts[0]
                            ),
                        });
                    }
                }
            }
        }

        if let Some(ms) = gw.get("max_sessions") {
            if let Some(n) = ms.as_u64() {
                if n == 0 {
                    errors.push(ValidationError {
                        field: "gateway.max_sessions".into(),
                        message: "must be greater than 0".into(),
                    });
                }
            }
        }
    }

    // config_version – positive integer
    if let Some(v) = config.get("config_version") {
        if let Some(n) = v.as_u64() {
            if n == 0 {
                errors.push(ValidationError {
                    field: "config_version".into(),
                    message: "must be a positive integer".into(),
                });
            }
        } else {
            errors.push(ValidationError {
                field: "config_version".into(),
                message: "must be a positive integer".into(),
            });
        }
    }

    errors
}

/// Migrate a config value from an older schema version to the current one.
///
/// Returns `true` if any migration was applied.
pub fn migrate_config(config: &mut serde_json::Value) -> Result<bool> {
    let version = config
        .get("config_version")
        .and_then(|v| v.as_u64())
        .unwrap_or(1);

    if version >= 2 {
        return Ok(false);
    }

    let obj = config
        .as_object_mut()
        .context("config root must be an object")?;

    // v1 → v2: `system_prompt` was a top-level string.  In v2 it no longer
    // exists as a dedicated field (prompts live elsewhere), but we keep the
    // value under `_migrated_system_prompt` so nothing is lost.
    if let Some(sp) = obj.remove("system_prompt") {
        obj.insert("_migrated_system_prompt".into(), sp);
    }

    obj.insert("config_version".into(), serde_json::json!(2));

    Ok(true)
}

/// Apply environment-variable overrides to a config value.
///
/// Recognised variables:
/// - `BFCODE_MODEL`
/// - `BFCODE_TEMPERATURE`
/// - `BFCODE_PROVIDER`
pub fn apply_env_overrides(config: &mut serde_json::Value) {
    let obj = match config.as_object_mut() {
        Some(o) => o,
        None => return,
    };

    if let Ok(val) = std::env::var("BFCODE_MODEL") {
        obj.insert("model".into(), serde_json::Value::String(val));
    }

    if let Ok(val) = std::env::var("BFCODE_TEMPERATURE") {
        if let Ok(n) = val.parse::<f64>() {
            obj.insert(
                "temperature".into(),
                serde_json::Value::Number(
                    serde_json::Number::from_f64(n).unwrap_or_else(|| serde_json::Number::from(1)),
                ),
            );
        }
    }

    if let Ok(val) = std::env::var("BFCODE_PROVIDER") {
        obj.insert("provider".into(), serde_json::Value::String(val));
    }
}

/// Discover all config files that would participate in the merge.
///
/// Returned in priority order (lowest first):
/// 1. `~/.bfcode/config.json`
/// 2. `~/.bfcode/config.yaml`
/// 3. `.bfcode/config.json`
/// 4. `.bfcode/config.yaml`
pub fn find_config_files() -> Vec<ConfigSource> {
    let mut sources = Vec::new();

    // Global configs.
    if let Some(home) = dirs::home_dir() {
        let global_dir = home.join(".bfcode");
        push_if_exists(&mut sources, global_dir.join("config.json"), ConfigFormat::Json);
        push_if_exists(&mut sources, global_dir.join("config.yaml"), ConfigFormat::Yaml);
    }

    // Project-local configs.
    let local_dir = PathBuf::from(".bfcode");
    push_if_exists(&mut sources, local_dir.join("config.json"), ConfigFormat::Json);
    push_if_exists(&mut sources, local_dir.join("config.yaml"), ConfigFormat::Yaml);

    sources
}

/// Pretty-print merged config together with the list of sources that
/// contributed to it.
pub fn format_config_info(config: &FullConfig, sources: &[ConfigSource]) -> String {
    let mut out = String::new();

    out.push_str(&"Configuration sources:\n".bold().to_string());
    if sources.is_empty() {
        out.push_str("  (none — using built-in defaults)\n");
    } else {
        for (i, src) in sources.iter().enumerate() {
            let fmt = match src.format {
                ConfigFormat::Json => "JSON",
                ConfigFormat::Yaml => "YAML",
            };
            out.push_str(&format!(
                "  {}. {} ({})\n",
                i + 1,
                src.path.display(),
                fmt
            ));
        }
    }

    out.push('\n');
    out.push_str(&"Merged configuration:\n".bold().to_string());
    if let Ok(pretty) = serde_json::to_string_pretty(config) {
        for line in pretty.lines() {
            out.push_str(&format!("  {line}\n"));
        }
    }

    out
}

/// Create a starter config file at `path` in the requested format.
pub fn init_config(path: &Path, format: ConfigFormat) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }

    let content = match format {
        ConfigFormat::Json => {
            concat!(
                "{\n",
                "  // bfcode configuration file\n",
                "  // See documentation for all available options.\n",
                "\n",
                "  // Model to use for completions.\n",
                "  \"model\": \"claude-opus-4-6\",\n",
                "\n",
                "  // Sampling temperature (0.0 - 2.0).\n",
                "  \"temperature\": 1.0,\n",
                "\n",
                "  // Provider backend.\n",
                "  \"provider\": \"anthropic\",\n",
                "\n",
                "  // Include other config files (merged before this one).\n",
                "  // \"include\": [\"shared.yaml\"],\n",
                "\n",
                "  // Custom environment variables.\n",
                "  \"env\": {},\n",
                "\n",
                "  // Lifecycle hooks (list of hook objects).\n",
                "  \"hooks\": [],\n",
                "\n",
                "  // Schema version (do not change manually).\n",
                "  \"config_version\": 2\n",
                "}\n",
            )
            .to_string()
        }
        ConfigFormat::Yaml => {
            r##"# bfcode configuration file
# See documentation for all available options.

# Model to use for completions.
model: claude-opus-4-6

# Sampling temperature (0.0 - 2.0).
temperature: 1.0

# Provider backend.
provider: anthropic

# Schema version (do not change manually).
config_version: 2

# Include other config files (merged before this one).
# include:
#   - shared.yaml

# Gateway settings (uncomment to enable).
# gateway:
#   listen: 127.0.0.1:8080
#   tailscale: false

# Daemon settings (uncomment to enable).
# daemon:
#   respawn: true
#   max_respawns: 5
"##
            .to_string()
        }
    };

    std::fs::write(path, &content)
        .with_context(|| format!("writing config to {}", path.display()))?;

    println!(
        "{} created {}",
        "ok:".green().bold(),
        path.display()
    );

    Ok(())
}

/// Produce a simple textual diff between two JSON config values.
///
/// Shows added, removed, and changed keys at the top level.
pub fn config_diff(old: &serde_json::Value, new: &serde_json::Value) -> String {
    let mut out = String::new();

    let old_obj = old.as_object();
    let new_obj = new.as_object();

    let (old_map, new_map) = match (old_obj, new_obj) {
        (Some(o), Some(n)) => (o, n),
        _ => {
            // Fall back to pretty-printed comparison.
            let old_str = serde_json::to_string_pretty(old).unwrap_or_default();
            let new_str = serde_json::to_string_pretty(new).unwrap_or_default();
            if old_str == new_str {
                return "(no changes)\n".to_string();
            }
            out.push_str(&format!("- {old_str}\n+ {new_str}\n"));
            return out;
        }
    };

    // Collect all keys.
    let mut all_keys: Vec<&String> = old_map.keys().chain(new_map.keys()).collect();
    all_keys.sort();
    all_keys.dedup();

    let mut has_changes = false;

    for key in all_keys {
        match (old_map.get(key), new_map.get(key)) {
            (None, Some(v)) => {
                has_changes = true;
                let v_str = serde_json::to_string(v).unwrap_or_default();
                out.push_str(&format!("{} {key}: {v_str}\n", "+".green()));
            }
            (Some(v), None) => {
                has_changes = true;
                let v_str = serde_json::to_string(v).unwrap_or_default();
                out.push_str(&format!("{} {key}: {v_str}\n", "-".red()));
            }
            (Some(a), Some(b)) if a != b => {
                has_changes = true;
                let a_str = serde_json::to_string(a).unwrap_or_default();
                let b_str = serde_json::to_string(b).unwrap_or_default();
                out.push_str(&format!("{} {key}: {a_str}\n", "-".red()));
                out.push_str(&format!("{} {key}: {b_str}\n", "+".green()));
            }
            _ => {} // unchanged
        }
    }

    if !has_changes {
        out.push_str("(no changes)\n");
    }

    out
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn push_if_exists(sources: &mut Vec<ConfigSource>, path: PathBuf, format: ConfigFormat) {
    if path.exists() {
        sources.push(ConfigSource { path, format });
    }
}

fn detect_format(path: &Path) -> ConfigFormat {
    match path.extension().and_then(|e| e.to_str()) {
        Some("yaml" | "yml") => ConfigFormat::Yaml,
        _ => ConfigFormat::Json,
    }
}

// ---------------------------------------------------------------------------
// Simple YAML parser
// ---------------------------------------------------------------------------

/// Parse a block of YAML lines starting at `line_idx` with the given
/// expected `indent` level.  Returns the parsed value and the next line
/// index to continue from.
fn parse_yaml_block(
    lines: &[&str],
    start: usize,
    indent: usize,
) -> Result<(serde_json::Value, usize)> {
    let mut map = serde_json::Map::new();
    let mut idx = start;

    while idx < lines.len() {
        let line = lines[idx];

        // Skip blank lines and comments.
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            idx += 1;
            continue;
        }

        // Determine indentation of this line.
        let line_indent = line.len() - line.trim_start().len();

        // If this line is *less* indented than expected, the block is done.
        if line_indent < indent {
            break;
        }

        // If this line is *more* indented than expected it is unexpected at
        // the top of a block (arrays handled below).
        if line_indent > indent && map.is_empty() && start == 0 && indent == 0 {
            // Tolerate extra indentation at the root level by adjusting.
            return parse_yaml_block(lines, start, line_indent);
        }
        if line_indent > indent {
            break;
        }

        // Expect "key: value" or "key:" (block mapping).
        let content = &line[indent..];

        if let Some(colon_pos) = find_colon_in_yaml_line(content) {
            let key = content[..colon_pos].trim().to_string();
            let after_colon = content[colon_pos + 1..].trim();

            if after_colon.is_empty() {
                // Could be a nested object or an array.
                idx += 1;

                // Peek at next non-empty/non-comment line to decide.
                let next = peek_next_content_line(lines, idx);
                match next {
                    Some((next_idx, next_line)) => {
                        let next_indent = next_line.len() - next_line.trim_start().len();
                        if next_indent <= indent {
                            // Empty value — treat as null.
                            map.insert(key, serde_json::Value::Null);
                        } else if next_line.trim_start().starts_with('-') {
                            // Array
                            let (arr, after) = parse_yaml_array(lines, next_idx, next_indent)?;
                            map.insert(key, arr);
                            idx = after;
                        } else {
                            // Nested object
                            let (obj, after) = parse_yaml_block(lines, next_idx, next_indent)?;
                            map.insert(key, obj);
                            idx = after;
                        }
                    }
                    None => {
                        map.insert(key, serde_json::Value::Null);
                    }
                }
            } else {
                // Inline value.
                let value = parse_yaml_scalar(after_colon);
                map.insert(key, value);
                idx += 1;
            }
        } else {
            // Unrecognised line — skip with warning.
            idx += 1;
        }
    }

    Ok((serde_json::Value::Object(map), idx))
}

/// Parse a YAML array (lines starting with `- `).
fn parse_yaml_array(
    lines: &[&str],
    start: usize,
    indent: usize,
) -> Result<(serde_json::Value, usize)> {
    let mut arr = Vec::new();
    let mut idx = start;

    while idx < lines.len() {
        let line = lines[idx];
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with('#') {
            idx += 1;
            continue;
        }

        let line_indent = line.len() - line.trim_start().len();
        if line_indent < indent {
            break;
        }
        if line_indent > indent {
            break;
        }

        let content = line[indent..].trim_start();
        if let Some(rest) = content.strip_prefix('-') {
            let val_str = rest.trim();
            if val_str.is_empty() {
                // Could be a nested structure under the dash — for simplicity
                // treat as empty string.
                arr.push(serde_json::Value::String(String::new()));
            } else {
                arr.push(parse_yaml_scalar(val_str));
            }
            idx += 1;
        } else {
            break;
        }
    }

    Ok((serde_json::Value::Array(arr), idx))
}

/// Parse a scalar YAML value (string / number / bool / null).
fn parse_yaml_scalar(s: &str) -> serde_json::Value {
    // Quoted strings.
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        let inner = &s[1..s.len() - 1];
        return serde_json::Value::String(inner.to_string());
    }

    // Booleans.
    match s.to_lowercase().as_str() {
        "true" | "yes" | "on" => return serde_json::Value::Bool(true),
        "false" | "no" | "off" => return serde_json::Value::Bool(false),
        "null" | "~" => return serde_json::Value::Null,
        _ => {}
    }

    // Integer.
    if let Ok(n) = s.parse::<i64>() {
        return serde_json::Value::Number(n.into());
    }

    // Float.
    if let Ok(n) = s.parse::<f64>() {
        if let Some(num) = serde_json::Number::from_f64(n) {
            return serde_json::Value::Number(num);
        }
    }

    // Default: string.
    serde_json::Value::String(s.to_string())
}

/// Find the position of the first `:` that acts as a key-value separator
/// (not inside quotes).
fn find_colon_in_yaml_line(s: &str) -> Option<usize> {
    let mut in_quote: Option<char> = None;
    for (i, ch) in s.char_indices() {
        match in_quote {
            Some(q) if ch == q => in_quote = None,
            Some(_) => {}
            None if ch == '"' || ch == '\'' => in_quote = Some(ch),
            None if ch == ':' => return Some(i),
            _ => {}
        }
    }
    None
}

/// Peek ahead to find the next non-blank, non-comment line.
fn peek_next_content_line<'a>(lines: &[&'a str], from: usize) -> Option<(usize, &'a str)> {
    for i in from..lines.len() {
        let t = lines[i].trim();
        if !t.is_empty() && !t.starts_with('#') {
            return Some((i, lines[i]));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_yaml_scalars() {
        let yaml = r#"
model: claude-opus-4-6
temperature: 0.7
provider: anthropic
config_version: 2
tailscale: true
"#;
        let val = parse_simple_yaml(yaml).unwrap();
        assert_eq!(val["model"], "claude-opus-4-6");
        assert_eq!(val["temperature"], 0.7);
        assert_eq!(val["provider"], "anthropic");
        assert_eq!(val["config_version"], 2);
        assert_eq!(val["tailscale"], true);
    }

    #[test]
    fn test_parse_yaml_nested() {
        let yaml = r#"
model: test
gateway:
  listen: 127.0.0.1:8080
  tailscale: false
"#;
        let val = parse_simple_yaml(yaml).unwrap();
        assert_eq!(val["model"], "test");
        assert_eq!(val["gateway"]["listen"], "127.0.0.1:8080");
        assert_eq!(val["gateway"]["tailscale"], false);
    }

    #[test]
    fn test_parse_yaml_array() {
        let yaml = r#"
model: test
include:
  - base.yaml
  - extra.json
"#;
        let val = parse_simple_yaml(yaml).unwrap();
        let arr = val["include"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0], "base.yaml");
        assert_eq!(arr[1], "extra.json");
    }

    #[test]
    fn test_parse_yaml_comments() {
        let yaml = r#"
# This is a comment
model: test
# Another comment
temperature: 1.0
"#;
        let val = parse_simple_yaml(yaml).unwrap();
        assert_eq!(val["model"], "test");
        assert_eq!(val["temperature"], 1.0);
    }

    #[test]
    fn test_parse_yaml_quoted() {
        let yaml = r#"
key1: "hello world"
key2: 'single quoted'
"#;
        let val = parse_simple_yaml(yaml).unwrap();
        assert_eq!(val["key1"], "hello world");
        assert_eq!(val["key2"], "single quoted");
    }

    #[test]
    fn test_merge_configs_deep() {
        let base = serde_json::json!({
            "model": "base",
            "gateway": { "listen": "0.0.0.0:80" },
            "hooks": ["a"]
        });
        let overlay = serde_json::json!({
            "model": "overlay",
            "gateway": { "tailscale": true },
            "hooks": ["b"]
        });
        let merged = merge_configs(base, overlay);
        assert_eq!(merged["model"], "overlay");
        assert_eq!(merged["gateway"]["listen"], "0.0.0.0:80");
        assert_eq!(merged["gateway"]["tailscale"], true);
        assert_eq!(merged["hooks"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_validate_temperature_range() {
        let config = serde_json::json!({ "temperature": 3.0 });
        let errs = validate_config(&config);
        assert!(errs.iter().any(|e| e.field == "temperature"));
    }

    #[test]
    fn test_validate_good_config() {
        let config = serde_json::json!({
            "model": "claude-opus-4-6",
            "temperature": 1.0,
            "config_version": 2
        });
        let errs = validate_config(&config);
        assert!(errs.is_empty());
    }

    #[test]
    fn test_validate_gateway_listen() {
        let config = serde_json::json!({
            "gateway": { "listen": "no-port" }
        });
        let errs = validate_config(&config);
        assert!(errs.iter().any(|e| e.field == "gateway.listen"));
    }

    #[test]
    fn test_migrate_v1_to_v2() {
        let mut config = serde_json::json!({
            "model": "old",
            "system_prompt": "you are helpful",
            "config_version": 1
        });
        let migrated = migrate_config(&mut config).unwrap();
        assert!(migrated);
        assert_eq!(config["config_version"], 2);
        assert!(config.get("system_prompt").is_none());
        assert_eq!(config["_migrated_system_prompt"], "you are helpful");
    }

    #[test]
    fn test_migrate_v2_noop() {
        let mut config = serde_json::json!({ "config_version": 2 });
        let migrated = migrate_config(&mut config).unwrap();
        assert!(!migrated);
    }

    #[test]
    fn test_apply_env_overrides() {
        let mut config = serde_json::json!({ "model": "original" });

        // Temporarily set env var.
        // SAFETY: This test is not run in parallel with other tests that
        // depend on this env var.
        unsafe {
            std::env::set_var("BFCODE_MODEL", "from-env");
        }
        apply_env_overrides(&mut config);
        assert_eq!(config["model"], "from-env");

        // Clean up.
        unsafe {
            std::env::remove_var("BFCODE_MODEL");
        }
    }

    #[test]
    fn test_config_diff_changes() {
        let old = serde_json::json!({ "model": "a", "temperature": 1.0 });
        let new = serde_json::json!({ "model": "b", "temperature": 1.0, "provider": "x" });
        let diff = config_diff(&old, &new);
        assert!(diff.contains("model"));
        assert!(diff.contains("provider"));
        // temperature is unchanged, should not appear.
        assert!(!diff.contains("temperature"));
    }

    #[test]
    fn test_config_diff_no_changes() {
        let val = serde_json::json!({ "model": "a" });
        let diff = config_diff(&val, &val);
        assert!(diff.contains("no changes"));
    }

    #[test]
    fn test_default_config_deserializes() {
        let val = default_config_value();
        let _config: FullConfig = serde_json::from_value(val).unwrap();
    }

    // -----------------------------------------------------------------------
    // Additional tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_yaml_nested_object() {
        let yaml = r#"
daemon:
  respawn: true
  max_respawns: 5
  log_file: /var/log/bfcode.log
gateway:
  listen: 0.0.0.0:9090
  tailscale: true
  max_sessions: 10
"#;
        let val = parse_simple_yaml(yaml).unwrap();
        assert_eq!(val["daemon"]["respawn"], true);
        assert_eq!(val["daemon"]["max_respawns"], 5);
        assert_eq!(val["daemon"]["log_file"], "/var/log/bfcode.log");
        assert_eq!(val["gateway"]["listen"], "0.0.0.0:9090");
        assert_eq!(val["gateway"]["tailscale"], true);
        assert_eq!(val["gateway"]["max_sessions"], 10);
    }

    #[test]
    fn test_parse_yaml_array_items() {
        let yaml = r#"
hooks:
  - pre-commit
  - post-push
  - deploy
api_keys:
  - key_abc123
  - key_def456
"#;
        let val = parse_simple_yaml(yaml).unwrap();
        let hooks = val["hooks"].as_array().unwrap();
        assert_eq!(hooks.len(), 3);
        assert_eq!(hooks[0], "pre-commit");
        assert_eq!(hooks[1], "post-push");
        assert_eq!(hooks[2], "deploy");
        let keys = val["api_keys"].as_array().unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0], "key_abc123");
    }

    #[test]
    fn test_parse_yaml_quoted_strings() {
        let yaml = r#"
double_quoted: "hello: world"
single_quoted: 'value with spaces'
colon_in_quotes: "host:port"
empty_double: ""
empty_single: ''
"#;
        let val = parse_simple_yaml(yaml).unwrap();
        assert_eq!(val["double_quoted"], "hello: world");
        assert_eq!(val["single_quoted"], "value with spaces");
        assert_eq!(val["colon_in_quotes"], "host:port");
        assert_eq!(val["empty_double"], "");
        assert_eq!(val["empty_single"], "");
    }

    #[test]
    fn test_parse_yaml_booleans_and_numbers() {
        let yaml = r#"
flag_true: true
flag_false: false
flag_yes: yes
flag_no: no
count: 42
pi: 3.14
nothing: null
tilde_null: ~
negative: -7
"#;
        let val = parse_simple_yaml(yaml).unwrap();
        assert_eq!(val["flag_true"], true);
        assert_eq!(val["flag_false"], false);
        assert_eq!(val["flag_yes"], true);
        assert_eq!(val["flag_no"], false);
        assert_eq!(val["count"], 42);
        assert_eq!(val["pi"], 3.14);
        assert!(val["nothing"].is_null());
        assert!(val["tilde_null"].is_null());
        assert_eq!(val["negative"], -7);
    }

    #[test]
    fn test_parse_yaml_comments_ignored() {
        let yaml = r#"
# top-level comment
model: test-model
# middle comment
temperature: 0.5
# trailing comment
"#;
        let val = parse_simple_yaml(yaml).unwrap();
        let obj = val.as_object().unwrap();
        // Only two keys should be present — comments must not produce entries.
        assert_eq!(obj.len(), 2);
        assert_eq!(val["model"], "test-model");
        assert_eq!(val["temperature"], 0.5);
    }

    #[test]
    fn test_merge_configs_scalar_override() {
        let base = serde_json::json!({
            "model": "base-model",
            "temperature": 0.5,
            "provider": "anthropic"
        });
        let overlay = serde_json::json!({
            "model": "overlay-model",
            "temperature": 1.5
        });
        let merged = merge_configs(base, overlay);
        // Overlay scalars win.
        assert_eq!(merged["model"], "overlay-model");
        assert_eq!(merged["temperature"], 1.5);
        // Base-only keys survive.
        assert_eq!(merged["provider"], "anthropic");
    }

    #[test]
    fn test_merge_configs_nested_merge() {
        let base = serde_json::json!({
            "gateway": {
                "listen": "127.0.0.1:8080",
                "tailscale": false,
                "max_sessions": 4
            }
        });
        let overlay = serde_json::json!({
            "gateway": {
                "tailscale": true,
                "max_sessions": 16
            }
        });
        let merged = merge_configs(base, overlay);
        // Overlay values override.
        assert_eq!(merged["gateway"]["tailscale"], true);
        assert_eq!(merged["gateway"]["max_sessions"], 16);
        // Base-only nested key survives.
        assert_eq!(merged["gateway"]["listen"], "127.0.0.1:8080");
    }

    #[test]
    fn test_merge_configs_array_concat() {
        let base = serde_json::json!({
            "hooks": ["lint", "test"],
            "include": ["shared.yaml"]
        });
        let overlay = serde_json::json!({
            "hooks": ["deploy"],
            "include": ["extra.yaml", "local.yaml"]
        });
        let merged = merge_configs(base, overlay);
        let hooks = merged["hooks"].as_array().unwrap();
        assert_eq!(hooks.len(), 3);
        assert_eq!(hooks[0], "lint");
        assert_eq!(hooks[1], "test");
        assert_eq!(hooks[2], "deploy");
        let inc = merged["include"].as_array().unwrap();
        assert_eq!(inc.len(), 3);
    }

    #[test]
    fn test_validate_valid_config() {
        let config = serde_json::json!({
            "model": "claude-opus-4-6",
            "temperature": 0.7,
            "provider": "anthropic",
            "config_version": 2,
            "gateway": {
                "listen": "127.0.0.1:8080",
                "max_sessions": 8
            }
        });
        let errs = validate_config(&config);
        assert!(errs.is_empty(), "expected no errors, got: {:?}", errs.iter().map(|e| e.to_string()).collect::<Vec<_>>());
    }

    #[test]
    fn test_validate_invalid_temperature() {
        let config = serde_json::json!({ "temperature": 2.5 });
        let errs = validate_config(&config);
        assert!(errs.iter().any(|e| e.field == "temperature"), "expected temperature error");

        let config_neg = serde_json::json!({ "temperature": -0.1 });
        let errs_neg = validate_config(&config_neg);
        assert!(errs_neg.iter().any(|e| e.field == "temperature"), "expected temperature error for negative");
    }

    #[test]
    fn test_validate_empty_model() {
        let config = serde_json::json!({ "model": "" });
        let errs = validate_config(&config);
        assert!(errs.iter().any(|e| e.field == "model" && e.message.contains("non-empty")));
    }

    #[test]
    fn test_migrate_v1_to_v2_adds_version() {
        let mut config = serde_json::json!({
            "model": "legacy-model",
            "temperature": 0.8
        });
        // No config_version field at all — defaults to v1.
        let migrated = migrate_config(&mut config).unwrap();
        assert!(migrated);
        assert_eq!(config["config_version"], 2);
    }

    #[test]
    fn test_apply_env_overrides_model() {
        let mut config = serde_json::json!({
            "model": "original-model",
            "temperature": 1.0,
            "provider": "anthropic"
        });

        unsafe {
            std::env::set_var("BFCODE_MODEL", "env-model");
            std::env::set_var("BFCODE_TEMPERATURE", "0.3");
            std::env::set_var("BFCODE_PROVIDER", "openai");
        }
        apply_env_overrides(&mut config);
        assert_eq!(config["model"], "env-model");
        assert_eq!(config["temperature"], 0.3);
        assert_eq!(config["provider"], "openai");

        // Clean up.
        unsafe {
            std::env::remove_var("BFCODE_MODEL");
            std::env::remove_var("BFCODE_TEMPERATURE");
            std::env::remove_var("BFCODE_PROVIDER");
        }
    }

    #[test]
    fn test_config_diff_shows_changes() {
        let old = serde_json::json!({
            "model": "old-model",
            "temperature": 1.0,
            "provider": "anthropic"
        });
        let new = serde_json::json!({
            "model": "new-model",
            "temperature": 1.0,
            "extra": "added"
        });
        let diff = config_diff(&old, &new);
        // Changed key.
        assert!(diff.contains("model"));
        // Added key.
        assert!(diff.contains("extra"));
        // Removed key.
        assert!(diff.contains("provider"));
        // Unchanged key should NOT appear.
        assert!(!diff.contains("temperature"));
    }

    #[test]
    fn test_full_config_default() {
        let val = default_config_value();
        let config: FullConfig = serde_json::from_value(val).unwrap();
        assert_eq!(config.model, "claude-opus-4-6");
        assert_eq!(config.temperature, 1.0);
        assert_eq!(config.provider, "anthropic");
        assert_eq!(config.config_version, 2);
        assert!(config.hooks.is_empty());
        assert!(config.env.is_empty());
        assert!(config.include.is_empty());
        assert!(config.gateway.is_none());
        assert!(config.daemon.is_none());
    }
}
