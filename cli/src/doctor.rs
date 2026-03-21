//! Health/diagnostics module for the bfcode CLI.
//!
//! Provides the `doctor` command: system health checks, API connectivity
//! verification, dependency detection, and diagnostics info for bug reports.

use anyhow::Result;
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Result of a single health check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub name: String,
    pub status: CheckStatus,
    pub message: String,
    pub details: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

impl std::fmt::Display for CheckStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckStatus::Pass => write!(f, "\u{2713} PASS"),
            CheckStatus::Warn => write!(f, "\u{26a0} WARN"),
            CheckStatus::Fail => write!(f, "\u{2717} FAIL"),
        }
    }
}

/// System diagnostics info suitable for inclusion in bug reports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticsInfo {
    pub version: String,
    pub os: String,
    pub arch: String,
    pub rust_version: String,
    pub home_dir: String,
    pub config_dir: String,
    pub project_dir: String,
    pub model: String,
    pub provider: String,
    pub session_count: usize,
    pub memory_count: usize,
    pub skill_count: usize,
    pub cron_job_count: usize,
    pub disk_usage: String,
    pub api_keys_set: Vec<String>,
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Run **all** health checks and return the collected results.
pub async fn run_doctor() -> Vec<CheckResult> {
    let mut results = Vec::new();

    // Synchronous checks first.
    let sync_checks: Vec<CheckResult> = vec![
        check_config(),
        check_api_keys(),
        check_git(),
        check_tools(),
        check_chrome(),
        check_tts(),
        check_disk_space(),
        check_sessions(),
        check_memories(),
        check_skills(),
    ];
    results.extend(sync_checks);

    // Async checks (network).
    results.push(check_api_connectivity().await);
    results.push(check_network().await);

    results
}

/// Run a single check identified by `name` (case-insensitive).
pub async fn run_check(name: &str) -> Option<CheckResult> {
    let name_lower = name.to_lowercase();
    match name_lower.as_str() {
        "config" => Some(check_config()),
        "api_keys" | "api-keys" | "apikeys" => Some(check_api_keys()),
        "api_connectivity" | "api-connectivity" | "apiconnectivity" => {
            Some(check_api_connectivity().await)
        }
        "git" => Some(check_git()),
        "tools" => Some(check_tools()),
        "chrome" | "browser" => Some(check_chrome()),
        "tts" => Some(check_tts()),
        "disk_space" | "disk-space" | "disk" => Some(check_disk_space()),
        "sessions" => Some(check_sessions()),
        "memories" | "memory" => Some(check_memories()),
        "skills" => Some(check_skills()),
        "network" => Some(check_network().await),
        _ => None,
    }
}

/// Collect system diagnostics info for bug reports.
pub fn collect_diagnostics() -> DiagnosticsInfo {
    let config = crate::persistence::load_config();
    let provider = crate::types::detect_provider(&config.model);

    let home_dir = dirs::home_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<unknown>".into());

    let config_dir = dirs::home_dir()
        .map(|p| p.join(".bfcode").display().to_string())
        .unwrap_or_else(|| "<unknown>".into());

    let project_dir = std::env::current_dir()
        .map(|p| p.join(".bfcode").display().to_string())
        .unwrap_or_else(|_| "<unknown>".into());

    let rust_version = option_env!("RUSTC_VERSION")
        .map(String::from)
        .unwrap_or_else(|| {
            Command::new("rustc")
                .arg("--version")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "<unknown>".into())
        });

    let session_count = crate::persistence::list_sessions().len();

    let memory_count = crate::persistence::list_memories().len();

    let skill_count = crate::skill::load_skills().len();

    let cron_job_count = crate::cron::CronManager::load().list_jobs().len();

    let disk_usage = dir_size_human(&PathBuf::from(".bfcode"));

    let api_keys_set = detect_api_keys()
        .into_iter()
        .filter(|(_, set)| *set)
        .map(|(name, _)| name.to_string())
        .collect();

    DiagnosticsInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        arch: std::env::consts::ARCH.to_string(),
        rust_version,
        home_dir,
        config_dir,
        project_dir,
        model: config.model.clone(),
        provider: provider.to_string(),
        session_count,
        memory_count,
        skill_count,
        cron_job_count,
        disk_usage,
        api_keys_set,
    }
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// Format doctor results for coloured terminal display.
pub fn format_doctor_results(results: &[CheckResult]) -> String {
    let mut out = String::new();
    out.push_str(&format!("{}\n\n", "bfcode doctor".bold()));

    for r in results {
        let status_str = match r.status {
            CheckStatus::Pass => format!("{}", "\u{2713} PASS".green()),
            CheckStatus::Warn => format!("{}", "\u{26a0} WARN".yellow()),
            CheckStatus::Fail => format!("{}", "\u{2717} FAIL".red()),
        };
        out.push_str(&format!("  {} {}: {}\n", status_str, r.name.bold(), r.message));
        if let Some(ref details) = r.details {
            for line in details.lines() {
                out.push_str(&format!("         {}\n", line.dimmed()));
            }
        }
    }

    let passed = results.iter().filter(|r| r.status == CheckStatus::Pass).count();
    let warned = results.iter().filter(|r| r.status == CheckStatus::Warn).count();
    let failed = results.iter().filter(|r| r.status == CheckStatus::Fail).count();

    out.push_str(&format!(
        "\n{}: {} passed, {} warnings, {} failed\n",
        "Summary".bold(),
        format!("{passed}").green(),
        format!("{warned}").yellow(),
        format!("{failed}").red(),
    ));

    out
}

/// Format diagnostics as plain text suitable for copying into a bug report.
pub fn format_diagnostics(info: &DiagnosticsInfo) -> String {
    let mut out = String::new();
    out.push_str(&format!("{}\n\n", "bfcode diagnostics".bold()));
    out.push_str(&format!("  {:<18} {}\n", "Version:", info.version));
    out.push_str(&format!("  {:<18} {}\n", "OS:", info.os));
    out.push_str(&format!("  {:<18} {}\n", "Arch:", info.arch));
    out.push_str(&format!("  {:<18} {}\n", "Rust:", info.rust_version));
    out.push_str(&format!("  {:<18} {}\n", "Home dir:", info.home_dir));
    out.push_str(&format!("  {:<18} {}\n", "Config dir:", info.config_dir));
    out.push_str(&format!("  {:<18} {}\n", "Project dir:", info.project_dir));
    out.push_str(&format!("  {:<18} {}\n", "Model:", info.model));
    out.push_str(&format!("  {:<18} {}\n", "Provider:", info.provider));
    out.push_str(&format!("  {:<18} {}\n", "Sessions:", info.session_count));
    out.push_str(&format!("  {:<18} {}\n", "Memories:", info.memory_count));
    out.push_str(&format!("  {:<18} {}\n", "Skills:", info.skill_count));
    out.push_str(&format!("  {:<18} {}\n", "Cron jobs:", info.cron_job_count));
    out.push_str(&format!("  {:<18} {}\n", "Disk usage:", info.disk_usage));

    if info.api_keys_set.is_empty() {
        out.push_str(&format!("  {:<18} {}\n", "API keys:", "(none)".red()));
    } else {
        out.push_str(&format!(
            "  {:<18} {}\n",
            "API keys:",
            info.api_keys_set.join(", ")
        ));
    }

    out
}

// ---------------------------------------------------------------------------
// Individual checks
// ---------------------------------------------------------------------------

/// Check: Configuration files exist and are valid JSON.
fn check_config() -> CheckResult {
    let config_path = dirs::home_dir()
        .map(|h| h.join(".bfcode").join("config.json"))
        .unwrap_or_else(|| PathBuf::from(".bfcode/config.json"));

    if !config_path.exists() {
        return CheckResult {
            name: "config".into(),
            status: CheckStatus::Warn,
            message: "config.json not found; using defaults".into(),
            details: Some(format!("Expected at {}", config_path.display())),
        };
    }

    match std::fs::read_to_string(&config_path) {
        Ok(data) => match serde_json::from_str::<serde_json::Value>(&data) {
            Ok(_) => CheckResult {
                name: "config".into(),
                status: CheckStatus::Pass,
                message: "config.json is valid".into(),
                details: Some(format!("{}", config_path.display())),
            },
            Err(e) => CheckResult {
                name: "config".into(),
                status: CheckStatus::Fail,
                message: "config.json is corrupt".into(),
                details: Some(format!("{}: {e}", config_path.display())),
            },
        },
        Err(e) => CheckResult {
            name: "config".into(),
            status: CheckStatus::Fail,
            message: "cannot read config.json".into(),
            details: Some(format!("{}: {e}", config_path.display())),
        },
    }
}

/// Check: API keys are set in the environment.
fn check_api_keys() -> CheckResult {
    let keys = detect_api_keys();
    let set_keys: Vec<&str> = keys.iter().filter(|(_, s)| *s).map(|(n, _)| *n).collect();
    let llm_keys: Vec<&str> = set_keys
        .iter()
        .copied()
        .filter(|k| *k == "GROK_API_KEY" || *k == "OPENAI_API_KEY" || *k == "ANTHROPIC_API_KEY")
        .collect();

    if llm_keys.is_empty() {
        CheckResult {
            name: "api_keys".into(),
            status: CheckStatus::Fail,
            message: "no LLM API keys found".into(),
            details: Some(
                "Set at least one of: GROK_API_KEY, OPENAI_API_KEY, ANTHROPIC_API_KEY".into(),
            ),
        }
    } else if set_keys.len() < keys.len() {
        let missing: Vec<&str> = keys.iter().filter(|(_, s)| !*s).map(|(n, _)| *n).collect();
        CheckResult {
            name: "api_keys".into(),
            status: CheckStatus::Warn,
            message: format!("{} of {} keys set", set_keys.len(), keys.len()),
            details: Some(format!(
                "Set: {}\nMissing: {}",
                set_keys.join(", "),
                missing.join(", ")
            )),
        }
    } else {
        CheckResult {
            name: "api_keys".into(),
            status: CheckStatus::Pass,
            message: format!("all {} API keys set", keys.len()),
            details: Some(set_keys.join(", ")),
        }
    }
}

/// Check: Can reach API endpoints (Grok, OpenAI, Anthropic).
async fn check_api_connectivity() -> CheckResult {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return CheckResult {
                name: "api_connectivity".into(),
                status: CheckStatus::Fail,
                message: "failed to build HTTP client".into(),
                details: Some(e.to_string()),
            };
        }
    };

    let endpoints = [
        ("Grok (x.ai)", "https://api.x.ai/v1/models"),
        ("OpenAI", "https://api.openai.com/v1/models"),
        ("Anthropic", "https://api.anthropic.com/v1/messages"),
    ];

    let mut reachable = Vec::new();
    let mut unreachable = Vec::new();

    for (label, url) in &endpoints {
        match client.get(*url).send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                // Any response (even 401/403) means the endpoint is reachable.
                if status < 500 {
                    reachable.push(*label);
                } else {
                    unreachable.push(format!("{label} (HTTP {status})"));
                }
            }
            Err(e) => {
                unreachable.push(format!("{label} ({e})"));
            }
        }
    }

    if unreachable.is_empty() {
        CheckResult {
            name: "api_connectivity".into(),
            status: CheckStatus::Pass,
            message: format!("all {} endpoints reachable", reachable.len()),
            details: Some(reachable.join(", ")),
        }
    } else if !reachable.is_empty() {
        CheckResult {
            name: "api_connectivity".into(),
            status: CheckStatus::Warn,
            message: format!(
                "{} reachable, {} unreachable",
                reachable.len(),
                unreachable.len()
            ),
            details: Some(format!(
                "Reachable: {}\nUnreachable: {}",
                reachable.join(", "),
                unreachable.join(", ")
            )),
        }
    } else {
        CheckResult {
            name: "api_connectivity".into(),
            status: CheckStatus::Fail,
            message: "no API endpoints reachable".into(),
            details: Some(unreachable.join(", ")),
        }
    }
}

/// Check: Git is available.
fn check_git() -> CheckResult {
    match Command::new("git").arg("--version").output() {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            CheckResult {
                name: "git".into(),
                status: CheckStatus::Pass,
                message: version,
                details: None,
            }
        }
        Ok(output) => CheckResult {
            name: "git".into(),
            status: CheckStatus::Fail,
            message: "git returned an error".into(),
            details: Some(String::from_utf8_lossy(&output.stderr).trim().to_string()),
        },
        Err(e) => CheckResult {
            name: "git".into(),
            status: CheckStatus::Fail,
            message: "git not found".into(),
            details: Some(e.to_string()),
        },
    }
}

/// Check: grep/find tools available.
fn check_tools() -> CheckResult {
    let mut available = Vec::new();
    let mut missing = Vec::new();

    for tool in &["grep", "find", "curl"] {
        match Command::new(tool).arg("--version").output() {
            Ok(o) if o.status.success() => available.push(*tool),
            // Some tools (e.g. macOS find) don't support --version but still exist.
            Ok(_) => {
                // Try a harmless invocation to confirm the binary exists.
                match Command::new("which").arg(tool).output() {
                    Ok(o) if o.status.success() => available.push(*tool),
                    _ => missing.push(*tool),
                }
            }
            Err(_) => missing.push(*tool),
        }
    }

    if missing.is_empty() {
        CheckResult {
            name: "tools".into(),
            status: CheckStatus::Pass,
            message: format!("all tools available ({})", available.join(", ")),
            details: None,
        }
    } else {
        CheckResult {
            name: "tools".into(),
            status: CheckStatus::Warn,
            message: format!("missing: {}", missing.join(", ")),
            details: Some(format!("Available: {}", available.join(", "))),
        }
    }
}

/// Check: Chrome/Chromium is installed (for browser tools).
fn check_chrome() -> CheckResult {
    let candidates: &[&str] = if cfg!(target_os = "macos") {
        &[
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        ]
    } else if cfg!(target_os = "linux") {
        &[
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/snap/bin/chromium",
        ]
    } else {
        // Windows or other — just check PATH.
        &[]
    };

    // Check known paths.
    for path in candidates {
        if Path::new(path).exists() {
            return CheckResult {
                name: "chrome".into(),
                status: CheckStatus::Pass,
                message: "browser found".into(),
                details: Some(path.to_string()),
            };
        }
    }

    // Fall back to PATH lookup.
    for name in &["google-chrome", "chromium", "chromium-browser"] {
        if Command::new("which").arg(name).output().map(|o| o.status.success()).unwrap_or(false) {
            return CheckResult {
                name: "chrome".into(),
                status: CheckStatus::Pass,
                message: format!("{name} found in PATH"),
                details: None,
            };
        }
    }

    CheckResult {
        name: "chrome".into(),
        status: CheckStatus::Warn,
        message: "Chrome/Chromium not found".into(),
        details: Some("Browser tools (playwright) will not work without a Chromium-based browser".into()),
    }
}

/// Check: TTS available (`say` on macOS, `espeak` on Linux).
fn check_tts() -> CheckResult {
    let candidates: &[&str] = if cfg!(target_os = "macos") {
        &["say"]
    } else {
        &["espeak", "espeak-ng"]
    };

    for cmd in candidates {
        let found = Command::new("which")
            .arg(cmd)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if found {
            return CheckResult {
                name: "tts".into(),
                status: CheckStatus::Pass,
                message: format!("{cmd} available"),
                details: None,
            };
        }
    }

    CheckResult {
        name: "tts".into(),
        status: CheckStatus::Warn,
        message: "no TTS engine found".into(),
        details: Some(if cfg!(target_os = "macos") {
            "Expected `say` (built-in on macOS)".into()
        } else {
            "Install espeak or espeak-ng for TTS support".into()
        }),
    }
}

/// Check: Disk space used by `.bfcode/`.
fn check_disk_space() -> CheckResult {
    let bfcode_dir = PathBuf::from(".bfcode");
    if !bfcode_dir.exists() {
        return CheckResult {
            name: "disk_space".into(),
            status: CheckStatus::Pass,
            message: ".bfcode/ does not exist yet (0 bytes)".into(),
            details: None,
        };
    }

    let bytes = dir_size_bytes(&bfcode_dir);
    let human = format_bytes(bytes);
    let threshold = 100 * 1024 * 1024; // 100 MB

    if bytes > threshold {
        CheckResult {
            name: "disk_space".into(),
            status: CheckStatus::Warn,
            message: format!(".bfcode/ is {human} (> 100 MB)"),
            details: Some("Consider pruning old sessions with /session delete".into()),
        }
    } else {
        CheckResult {
            name: "disk_space".into(),
            status: CheckStatus::Pass,
            message: format!(".bfcode/ is {human}"),
            details: None,
        }
    }
}

/// Check: Session storage health.
fn check_sessions() -> CheckResult {
    let sessions_dir = PathBuf::from(".bfcode/sessions");
    if !sessions_dir.exists() {
        return CheckResult {
            name: "sessions".into(),
            status: CheckStatus::Pass,
            message: "no sessions directory (new project)".into(),
            details: None,
        };
    }

    let mut total = 0usize;
    let mut valid = 0usize;
    let mut corrupt = 0usize;

    if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                total += 1;
                match std::fs::read_to_string(&path) {
                    Ok(data) => {
                        if serde_json::from_str::<serde_json::Value>(&data).is_ok() {
                            valid += 1;
                        } else {
                            corrupt += 1;
                        }
                    }
                    Err(_) => corrupt += 1,
                }
            }
        }
    }

    if corrupt > 0 {
        CheckResult {
            name: "sessions".into(),
            status: CheckStatus::Warn,
            message: format!("{total} sessions ({corrupt} corrupt)"),
            details: Some(format!("{valid} valid, {corrupt} corrupt")),
        }
    } else {
        CheckResult {
            name: "sessions".into(),
            status: CheckStatus::Pass,
            message: format!("{total} sessions"),
            details: None,
        }
    }
}

/// Check: Memory files are valid.
fn check_memories() -> CheckResult {
    let memory_dir = PathBuf::from(".bfcode/memory");
    if !memory_dir.exists() {
        return CheckResult {
            name: "memories".into(),
            status: CheckStatus::Pass,
            message: "no memories directory".into(),
            details: None,
        };
    }

    let count = std::fs::read_dir(&memory_dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| {
                    e.path()
                        .extension()
                        .map(|ext| ext == "md")
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0);

    CheckResult {
        name: "memories".into(),
        status: CheckStatus::Pass,
        message: format!("{count} memory file(s)"),
        details: None,
    }
}

/// Check: Skills are loadable.
fn check_skills() -> CheckResult {
    let skills = crate::skill::load_skills();
    if skills.is_empty() {
        CheckResult {
            name: "skills".into(),
            status: CheckStatus::Warn,
            message: "no skills found".into(),
            details: Some(
                "Add .md files to ~/.bfcode/skills/ or .bfcode/skills/ to register skills".into(),
            ),
        }
    } else {
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).take(10).collect();
        CheckResult {
            name: "skills".into(),
            status: CheckStatus::Pass,
            message: format!("{} skill(s) loaded", skills.len()),
            details: Some(names.join(", ")),
        }
    }
}

/// Check: General network connectivity.
async fn check_network() -> CheckResult {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return CheckResult {
                name: "network".into(),
                status: CheckStatus::Fail,
                message: "failed to build HTTP client".into(),
                details: Some(e.to_string()),
            };
        }
    };

    match client.get("https://httpbin.org/get").send().await {
        Ok(resp) if resp.status().is_success() => CheckResult {
            name: "network".into(),
            status: CheckStatus::Pass,
            message: "internet reachable (httpbin.org)".into(),
            details: None,
        },
        Ok(resp) => CheckResult {
            name: "network".into(),
            status: CheckStatus::Warn,
            message: format!("httpbin.org returned HTTP {}", resp.status()),
            details: None,
        },
        Err(e) => CheckResult {
            name: "network".into(),
            status: CheckStatus::Fail,
            message: "cannot reach httpbin.org".into(),
            details: Some(e.to_string()),
        },
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Return a list of `(env_var_name, is_set)` for known API keys.
fn detect_api_keys() -> Vec<(&'static str, bool)> {
    [
        "GROK_API_KEY",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "BRAVE_API_KEY",
        "TAVILY_API_KEY",
    ]
    .iter()
    .map(|&name| (name, std::env::var(name).map(|v| !v.is_empty()).unwrap_or(false)))
    .collect()
}

/// Recursively compute the total size (in bytes) of a directory.
fn dir_size_bytes(path: &Path) -> u64 {
    let mut total: u64 = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += dir_size_bytes(&p);
            } else if let Ok(meta) = p.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

/// Human-readable size string for a directory.
fn dir_size_human(path: &Path) -> String {
    if !path.exists() {
        return "0 B".into();
    }
    format_bytes(dir_size_bytes(path))
}

/// Format a byte count as a human-readable string.
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_status_display() {
        assert_eq!(CheckStatus::Pass.to_string(), "\u{2713} PASS");
        assert_eq!(CheckStatus::Warn.to_string(), "\u{26a0} WARN");
        assert_eq!(CheckStatus::Fail.to_string(), "\u{2717} FAIL");
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1_500_000), "1.4 MB");
        assert_eq!(format_bytes(2_000_000_000), "1.9 GB");
    }

    #[test]
    fn test_detect_api_keys_returns_five() {
        let keys = detect_api_keys();
        assert_eq!(keys.len(), 5);
    }

    #[test]
    fn test_check_git_runs() {
        let result = check_git();
        // Git is virtually always available in dev environments.
        assert!(result.status == CheckStatus::Pass || result.status == CheckStatus::Fail);
    }

    #[test]
    fn test_format_doctor_results_summary() {
        let results = vec![
            CheckResult {
                name: "a".into(),
                status: CheckStatus::Pass,
                message: "ok".into(),
                details: None,
            },
            CheckResult {
                name: "b".into(),
                status: CheckStatus::Warn,
                message: "meh".into(),
                details: Some("hint".into()),
            },
            CheckResult {
                name: "c".into(),
                status: CheckStatus::Fail,
                message: "bad".into(),
                details: None,
            },
        ];
        let out = format_doctor_results(&results);
        assert!(out.contains("passed"));
        assert!(out.contains("warnings"));
        assert!(out.contains("failed"));
    }

    #[test]
    fn test_check_status_serde_roundtrip() {
        let json = serde_json::to_string(&CheckStatus::Pass).unwrap();
        assert_eq!(json, "\"pass\"");
        let parsed: CheckStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, CheckStatus::Pass);
    }

    #[tokio::test]
    async fn test_run_check_unknown_returns_none() {
        assert!(run_check("nonexistent_check").await.is_none());
    }

    #[tokio::test]
    async fn test_run_check_known_returns_some() {
        assert!(run_check("git").await.is_some());
        assert!(run_check("config").await.is_some());
    }

    #[test]
    fn test_check_result_pass() {
        let r = CheckResult {
            name: "test_pass".into(),
            status: CheckStatus::Pass,
            message: "everything is fine".into(),
            details: None,
        };
        assert_eq!(r.name, "test_pass");
        assert_eq!(r.status, CheckStatus::Pass);
        assert_eq!(r.message, "everything is fine");
        assert!(r.details.is_none());
    }

    #[test]
    fn test_check_result_fail() {
        let r = CheckResult {
            name: "test_fail".into(),
            status: CheckStatus::Fail,
            message: "something broke".into(),
            details: Some("error details here".into()),
        };
        assert_eq!(r.name, "test_fail");
        assert_eq!(r.status, CheckStatus::Fail);
        assert_eq!(r.message, "something broke");
        assert_eq!(r.details.as_deref(), Some("error details here"));
    }

    #[test]
    fn test_check_result_warn_with_details() {
        let r = CheckResult {
            name: "test_warn".into(),
            status: CheckStatus::Warn,
            message: "partially working".into(),
            details: Some("line1\nline2\nline3".into()),
        };
        assert_eq!(r.name, "test_warn");
        assert_eq!(r.status, CheckStatus::Warn);
        assert_eq!(r.message, "partially working");
        let details = r.details.unwrap();
        assert!(details.contains("line1"));
        assert!(details.contains("line2"));
        assert!(details.contains("line3"));
        assert_eq!(details.lines().count(), 3);
    }

    #[test]
    fn test_format_results_all_pass() {
        let results = vec![
            CheckResult {
                name: "alpha".into(),
                status: CheckStatus::Pass,
                message: "ok".into(),
                details: None,
            },
            CheckResult {
                name: "beta".into(),
                status: CheckStatus::Pass,
                message: "ok".into(),
                details: None,
            },
            CheckResult {
                name: "gamma".into(),
                status: CheckStatus::Pass,
                message: "ok".into(),
                details: None,
            },
        ];
        let out = format_doctor_results(&results);
        assert!(out.contains("passed"));
        assert!(out.contains("warnings"));
        assert!(out.contains("failed"));
        assert!(out.contains("alpha"));
        assert!(out.contains("beta"));
        assert!(out.contains("gamma"));
    }

    #[test]
    fn test_format_results_mixed() {
        let results = vec![
            CheckResult {
                name: "p1".into(),
                status: CheckStatus::Pass,
                message: "ok".into(),
                details: None,
            },
            CheckResult {
                name: "p2".into(),
                status: CheckStatus::Pass,
                message: "ok".into(),
                details: None,
            },
            CheckResult {
                name: "w1".into(),
                status: CheckStatus::Warn,
                message: "hmm".into(),
                details: None,
            },
            CheckResult {
                name: "f1".into(),
                status: CheckStatus::Fail,
                message: "bad".into(),
                details: None,
            },
            CheckResult {
                name: "f2".into(),
                status: CheckStatus::Fail,
                message: "bad".into(),
                details: None,
            },
        ];
        let out = format_doctor_results(&results);
        assert!(out.contains("passed"));
        assert!(out.contains("warnings"));
        assert!(out.contains("failed"));
    }

    #[test]
    fn test_format_results_empty() {
        let results: Vec<CheckResult> = vec![];
        let out = format_doctor_results(&results);
        assert!(out.contains("passed"));
        assert!(out.contains("warnings"));
        assert!(out.contains("failed"));
        assert!(out.contains("bfcode doctor"));
    }

    #[test]
    fn test_diagnostics_info_fields() {
        let info = DiagnosticsInfo {
            version: "1.2.3".into(),
            os: "linux x86_64".into(),
            arch: "x86_64".into(),
            rust_version: "rustc 1.75.0".into(),
            home_dir: "/home/user".into(),
            config_dir: "/home/user/.bfcode".into(),
            project_dir: "/tmp/project/.bfcode".into(),
            model: "grok-3".into(),
            provider: "xai".into(),
            session_count: 5,
            memory_count: 3,
            skill_count: 7,
            cron_job_count: 2,
            disk_usage: "4.2 MB".into(),
            api_keys_set: vec!["GROK_API_KEY".into(), "OPENAI_API_KEY".into()],
        };
        assert_eq!(info.version, "1.2.3");
        assert_eq!(info.os, "linux x86_64");
        assert_eq!(info.arch, "x86_64");
        assert_eq!(info.rust_version, "rustc 1.75.0");
        assert_eq!(info.home_dir, "/home/user");
        assert_eq!(info.config_dir, "/home/user/.bfcode");
        assert_eq!(info.project_dir, "/tmp/project/.bfcode");
        assert_eq!(info.model, "grok-3");
        assert_eq!(info.provider, "xai");
        assert_eq!(info.session_count, 5);
        assert_eq!(info.memory_count, 3);
        assert_eq!(info.skill_count, 7);
        assert_eq!(info.cron_job_count, 2);
        assert_eq!(info.disk_usage, "4.2 MB");
        assert_eq!(info.api_keys_set.len(), 2);
        assert!(info.api_keys_set.contains(&"GROK_API_KEY".to_string()));
    }

    #[test]
    fn test_format_diagnostics_contains_fields() {
        let info = DiagnosticsInfo {
            version: "0.9.0".into(),
            os: "macos aarch64".into(),
            arch: "aarch64".into(),
            rust_version: "rustc 1.80.0".into(),
            home_dir: "/Users/test".into(),
            config_dir: "/Users/test/.bfcode".into(),
            project_dir: "/tmp/.bfcode".into(),
            model: "gpt-4o".into(),
            provider: "openai".into(),
            session_count: 10,
            memory_count: 2,
            skill_count: 4,
            cron_job_count: 1,
            disk_usage: "512 B".into(),
            api_keys_set: vec!["OPENAI_API_KEY".into()],
        };
        let out = format_diagnostics(&info);
        assert!(out.contains("Version:"));
        assert!(out.contains("0.9.0"));
        assert!(out.contains("OS:"));
        assert!(out.contains("macos aarch64"));
        assert!(out.contains("Arch:"));
        assert!(out.contains("Rust:"));
        assert!(out.contains("Home dir:"));
        assert!(out.contains("Config dir:"));
        assert!(out.contains("Project dir:"));
        assert!(out.contains("Model:"));
        assert!(out.contains("gpt-4o"));
        assert!(out.contains("Provider:"));
        assert!(out.contains("openai"));
        assert!(out.contains("Sessions:"));
        assert!(out.contains("Memories:"));
        assert!(out.contains("Skills:"));
        assert!(out.contains("Cron jobs:"));
        assert!(out.contains("Disk usage:"));
        assert!(out.contains("API keys:"));
        assert!(out.contains("OPENAI_API_KEY"));
    }

    #[test]
    fn test_check_config_runs() {
        let result = check_config();
        assert_eq!(result.name, "config");
        assert!(
            result.status == CheckStatus::Pass
                || result.status == CheckStatus::Warn
                || result.status == CheckStatus::Fail
        );
        assert!(!result.message.is_empty());
    }

    #[test]
    fn test_check_api_keys_runs() {
        let result = check_api_keys();
        assert_eq!(result.name, "api_keys");
        assert!(
            result.status == CheckStatus::Pass
                || result.status == CheckStatus::Warn
                || result.status == CheckStatus::Fail
        );
        assert!(!result.message.is_empty());
    }

    #[test]
    fn test_check_git_runs_and_returns_result() {
        let result = check_git();
        assert_eq!(result.name, "git");
        assert!(
            result.status == CheckStatus::Pass || result.status == CheckStatus::Fail
        );
        assert!(!result.message.is_empty());
    }

    #[test]
    fn test_check_tools_runs() {
        let result = check_tools();
        assert_eq!(result.name, "tools");
        assert!(
            result.status == CheckStatus::Pass || result.status == CheckStatus::Warn
        );
        assert!(!result.message.is_empty());
    }
}
