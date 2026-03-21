use anyhow::{bail, Context, Result};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// GitHub repository for update checks.
const GITHUB_REPO: &str = "user/bfcode";

/// Current binary version (pulled from Cargo).
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Daemon configuration – lives under the `"daemon"` key in
/// `~/.bfcode/config.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// PID file path.
    #[serde(default = "default_pid_file")]
    pub pid_file: String,
    /// Log file path.
    #[serde(default = "default_log_file")]
    pub log_file: String,
    /// Auto-restart on crash.
    #[serde(default = "default_true")]
    pub respawn: bool,
    /// Max respawn attempts before giving up.
    #[serde(default = "default_max_respawns")]
    pub max_respawns: u32,
    /// Auto-update check interval in hours (0 = disabled).
    #[serde(default)]
    pub auto_update_hours: u32,
}

fn default_pid_file() -> String {
    dirs::home_dir()
        .map(|h| h.join(".bfcode/bfcode.pid").to_string_lossy().to_string())
        .unwrap_or_else(|| "/tmp/bfcode.pid".into())
}

fn default_log_file() -> String {
    dirs::home_dir()
        .map(|h| h.join(".bfcode/bfcode.log").to_string_lossy().to_string())
        .unwrap_or_else(|| "/tmp/bfcode.log".into())
}

fn default_true() -> bool {
    true
}

fn default_max_respawns() -> u32 {
    5
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            pid_file: default_pid_file(),
            log_file: default_log_file(),
            respawn: default_true(),
            max_respawns: default_max_respawns(),
            auto_update_hours: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Status / Update types
// ---------------------------------------------------------------------------

/// Snapshot of daemon health returned by [`daemon_status`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub uptime: Option<String>,
    pub log_file: String,
    pub version: String,
}

/// Information about an available update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateInfo {
    pub current_version: String,
    pub latest_version: String,
    pub download_url: String,
    pub release_notes: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read PID from the pid-file and return it if the process is alive.
fn read_running_pid(pid_file: &str) -> Option<u32> {
    let content = std::fs::read_to_string(pid_file).ok()?;
    let pid: u32 = content.trim().parse().ok()?;
    if process_alive(pid) {
        Some(pid)
    } else {
        None
    }
}

/// Check whether a process is still alive using `kill -0`.
fn process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Human-readable duration from seconds.
fn humanize_duration(secs: u64) -> String {
    if secs < 60 {
        return format!("{secs}s");
    }
    let minutes = secs / 60;
    if minutes < 60 {
        return format!("{minutes}m {}s", secs % 60);
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h {}m", minutes % 60);
    }
    let days = hours / 24;
    format!("{days}d {}h", hours % 24)
}

/// Ensure the parent directory of `path` exists.
fn ensure_parent(path: &str) -> Result<()> {
    if let Some(parent) = PathBuf::from(path).parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Daemon lifecycle
// ---------------------------------------------------------------------------

/// Start bfcode as a daemon (background process).
///
/// Spawns a child with `--daemon-child` that redirects stdout/stderr to the
/// configured log file and writes its PID to `pid_file`.  The current
/// (parent) process prints a success message and returns.
pub fn start_daemon(config: &DaemonConfig) -> Result<()> {
    // Check if already running.
    if let Some(pid) = read_running_pid(&config.pid_file) {
        bail!(
            "Daemon already running (PID {pid}). Stop it first with {}.",
            "bfcode daemon stop".yellow()
        );
    }

    ensure_parent(&config.pid_file)?;
    ensure_parent(&config.log_file)?;

    let exe = std::env::current_exe().context("failed to determine current executable")?;

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&config.log_file)
        .with_context(|| format!("failed to open log file {}", config.log_file))?;

    let log_err = log_file
        .try_clone()
        .context("failed to clone log file handle")?;

    let child = std::process::Command::new(&exe)
        .args(["gateway", "start", "--daemon-child"])
        .stdout(std::process::Stdio::from(log_file))
        .stderr(std::process::Stdio::from(log_err))
        .stdin(std::process::Stdio::null())
        .spawn()
        .context("failed to spawn daemon child process")?;

    let pid = child.id();
    std::fs::write(&config.pid_file, pid.to_string())
        .with_context(|| format!("failed to write PID file {}", config.pid_file))?;

    println!(
        "{} Daemon started (PID {pid}). Logs: {}",
        "✓".green().bold(),
        config.log_file.cyan()
    );

    Ok(())
}

/// Stop a running daemon by sending SIGTERM and removing the PID file.
pub fn stop_daemon(config: &DaemonConfig) -> Result<()> {
    let pid = read_running_pid(&config.pid_file);

    match pid {
        Some(pid) => {
            // Send SIGTERM.
            let status = std::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status()
                .context("failed to send SIGTERM")?;

            if !status.success() {
                bail!("kill -TERM {pid} failed (exit code {:?})", status.code());
            }

            // Remove PID file.
            let _ = std::fs::remove_file(&config.pid_file);

            println!("{} Daemon stopped (PID {pid}).", "✓".green().bold());
            Ok(())
        }
        None => {
            // Stale PID file – clean up.
            let _ = std::fs::remove_file(&config.pid_file);
            println!("{} No running daemon found.", "!".yellow().bold());
            Ok(())
        }
    }
}

/// Query the current daemon status without mutating anything.
pub fn daemon_status(config: &DaemonConfig) -> DaemonStatus {
    let pid = read_running_pid(&config.pid_file);
    let running = pid.is_some();

    let uptime = if running {
        std::fs::metadata(&config.pid_file)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|modified| {
                std::time::SystemTime::now()
                    .duration_since(modified)
                    .ok()
                    .map(|d| humanize_duration(d.as_secs()))
            })
    } else {
        None
    };

    DaemonStatus {
        running,
        pid,
        uptime,
        log_file: config.log_file.clone(),
        version: CURRENT_VERSION.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Auto-update
// ---------------------------------------------------------------------------

/// GitHub release payload (partial).
#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

/// Check GitHub releases for a newer version.
///
/// Returns `Ok(None)` when the local version is already up to date.
pub async fn check_for_updates() -> Result<Option<UpdateInfo>> {
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");

    let client = reqwest::Client::builder()
        .user_agent("bfcode-updater")
        .build()
        .context("failed to build HTTP client")?;

    let resp = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("failed to reach GitHub releases API")?;

    if !resp.status().is_success() {
        bail!(
            "GitHub API returned status {} for {}",
            resp.status(),
            url
        );
    }

    let release: GitHubRelease = resp
        .json()
        .await
        .context("failed to parse GitHub release JSON")?;

    let latest = release.tag_name.trim_start_matches('v').to_string();
    if latest == CURRENT_VERSION {
        return Ok(None);
    }

    // Attempt to pick an asset for the current platform.
    let target = current_platform_target();
    let download_url = release
        .assets
        .iter()
        .find(|a| a.name.contains(&target))
        .map(|a| a.browser_download_url.clone())
        .unwrap_or_default();

    Ok(Some(UpdateInfo {
        current_version: CURRENT_VERSION.to_string(),
        latest_version: latest,
        download_url,
        release_notes: release.body,
    }))
}

/// Self-update: download the new binary and replace the current executable.
///
/// This is a best-effort operation – the caller should handle failures
/// gracefully.
pub async fn self_update(info: &UpdateInfo) -> Result<()> {
    if info.download_url.is_empty() {
        bail!("No download URL available for the current platform");
    }

    println!(
        "{} Downloading {} ...",
        "↓".cyan().bold(),
        info.latest_version.yellow()
    );

    let client = reqwest::Client::builder()
        .user_agent("bfcode-updater")
        .build()
        .context("failed to build HTTP client")?;

    let bytes = client
        .get(&info.download_url)
        .send()
        .await
        .context("download request failed")?
        .bytes()
        .await
        .context("failed to read download body")?;

    let current_exe = std::env::current_exe().context("cannot determine current executable")?;
    let tmp_path = current_exe.with_extension("update-tmp");

    std::fs::write(&tmp_path, &bytes)
        .with_context(|| format!("failed to write temp binary to {}", tmp_path.display()))?;

    // Make executable on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755))
            .context("failed to set executable permission")?;
    }

    // Atomic-ish rename.
    let backup = current_exe.with_extension("old");
    let _ = std::fs::remove_file(&backup);
    std::fs::rename(&current_exe, &backup)
        .context("failed to back up current binary")?;
    std::fs::rename(&tmp_path, &current_exe)
        .context("failed to move new binary into place")?;
    let _ = std::fs::remove_file(&backup);

    println!(
        "{} Updated to {} successfully.",
        "✓".green().bold(),
        info.latest_version.green()
    );

    Ok(())
}

/// Return a target-triple-ish string for asset matching.
fn current_platform_target() -> String {
    let os = if cfg!(target_os = "macos") {
        "apple-darwin"
    } else if cfg!(target_os = "linux") {
        "unknown-linux"
    } else if cfg!(target_os = "windows") {
        "pc-windows"
    } else {
        "unknown"
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "unknown"
    };
    format!("{arch}-{os}")
}

// ---------------------------------------------------------------------------
// Service file generation & installation
// ---------------------------------------------------------------------------

/// Generate a systemd user-unit file for Linux.
pub fn generate_systemd_unit() -> Result<String> {
    let exe = std::env::current_exe()
        .context("failed to determine current executable")?
        .to_string_lossy()
        .to_string();

    let user = std::env::var("USER").unwrap_or_else(|_| "%u".into());

    Ok(format!(
        r#"[Unit]
Description=bfcode AI coding assistant daemon
After=network.target

[Service]
Type=simple
ExecStart={exe} gateway start
Restart=on-failure
RestartSec=5
User={user}
Environment=HOME=%h

[Install]
WantedBy=default.target
"#
    ))
}

/// Generate a launchd plist for macOS.
pub fn generate_launchd_plist() -> Result<String> {
    let exe = std::env::current_exe()
        .context("failed to determine current executable")?
        .to_string_lossy()
        .to_string();

    let log_dir = dirs::home_dir()
        .map(|h| h.join(".bfcode").to_string_lossy().to_string())
        .unwrap_or_else(|| "/tmp".into());

    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.bfcode.daemon</string>

    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>gateway</string>
        <string>start</string>
    </array>

    <key>RunAtLoad</key>
    <true/>

    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>

    <key>StandardOutPath</key>
    <string>{log_dir}/bfcode.log</string>

    <key>StandardErrorPath</key>
    <string>{log_dir}/bfcode.err.log</string>

    <key>ThrottleInterval</key>
    <integer>5</integer>
</dict>
</plist>
"#
    ))
}

/// Install the platform-appropriate service file.
///
/// Returns a message describing what was done.
pub fn install_service() -> Result<String> {
    if cfg!(target_os = "macos") {
        install_launchd_service()
    } else if cfg!(target_os = "linux") {
        install_systemd_service()
    } else {
        bail!("Service installation is only supported on macOS and Linux");
    }
}

fn install_launchd_service() -> Result<String> {
    let plist_content = generate_launchd_plist()?;
    let plist_dir = dirs::home_dir()
        .context("cannot determine home directory")?
        .join("Library/LaunchAgents");
    std::fs::create_dir_all(&plist_dir)
        .with_context(|| format!("failed to create {}", plist_dir.display()))?;

    let plist_path = plist_dir.join("com.bfcode.daemon.plist");
    std::fs::write(&plist_path, &plist_content)
        .with_context(|| format!("failed to write {}", plist_path.display()))?;

    let status = std::process::Command::new("launchctl")
        .args(["load", "-w"])
        .arg(&plist_path)
        .status()
        .context("failed to run launchctl load")?;

    if !status.success() {
        bail!("launchctl load failed (exit code {:?})", status.code());
    }

    Ok(format!(
        "Installed launchd service at {}.\nThe daemon will start automatically on login.",
        plist_path.display()
    ))
}

fn install_systemd_service() -> Result<String> {
    let unit_content = generate_systemd_unit()?;
    let unit_dir = dirs::home_dir()
        .context("cannot determine home directory")?
        .join(".config/systemd/user");
    std::fs::create_dir_all(&unit_dir)
        .with_context(|| format!("failed to create {}", unit_dir.display()))?;

    let unit_path = unit_dir.join("bfcode.service");
    std::fs::write(&unit_path, &unit_content)
        .with_context(|| format!("failed to write {}", unit_path.display()))?;

    let enable = std::process::Command::new("systemctl")
        .args(["--user", "enable", "bfcode.service"])
        .status()
        .context("failed to run systemctl --user enable")?;

    if !enable.success() {
        bail!(
            "systemctl --user enable failed (exit code {:?})",
            enable.code()
        );
    }

    let start = std::process::Command::new("systemctl")
        .args(["--user", "start", "bfcode.service"])
        .status()
        .context("failed to run systemctl --user start")?;

    if !start.success() {
        eprintln!(
            "{} systemctl start returned non-zero – the service may not be running yet.",
            "!".yellow().bold()
        );
    }

    Ok(format!(
        "Installed systemd user service at {}.\n\
         Enabled and started. Use `systemctl --user status bfcode` to check.",
        unit_path.display()
    ))
}

/// Uninstall the platform service file.
///
/// Returns a message describing what was done.
pub fn uninstall_service() -> Result<String> {
    if cfg!(target_os = "macos") {
        uninstall_launchd_service()
    } else if cfg!(target_os = "linux") {
        uninstall_systemd_service()
    } else {
        bail!("Service uninstallation is only supported on macOS and Linux");
    }
}

fn uninstall_launchd_service() -> Result<String> {
    let plist_path = dirs::home_dir()
        .context("cannot determine home directory")?
        .join("Library/LaunchAgents/com.bfcode.daemon.plist");

    if plist_path.exists() {
        let _ = std::process::Command::new("launchctl")
            .args(["unload", "-w"])
            .arg(&plist_path)
            .status();

        std::fs::remove_file(&plist_path)
            .with_context(|| format!("failed to remove {}", plist_path.display()))?;
    }

    Ok(format!(
        "Removed launchd service ({}).",
        plist_path.display()
    ))
}

fn uninstall_systemd_service() -> Result<String> {
    let unit_path = dirs::home_dir()
        .context("cannot determine home directory")?
        .join(".config/systemd/user/bfcode.service");

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "stop", "bfcode.service"])
        .status();
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "disable", "bfcode.service"])
        .status();

    if unit_path.exists() {
        std::fs::remove_file(&unit_path)
            .with_context(|| format!("failed to remove {}", unit_path.display()))?;
    }

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();

    Ok(format!(
        "Stopped, disabled, and removed systemd service ({}).",
        unit_path.display()
    ))
}

// ---------------------------------------------------------------------------
// Display
// ---------------------------------------------------------------------------

/// Pretty-print a [`DaemonStatus`] for terminal output.
pub fn format_status(status: &DaemonStatus) -> String {
    let mut lines = Vec::new();

    lines.push(format!(
        "{} {}",
        "bfcode daemon".bold(),
        format!("v{}", status.version).dimmed()
    ));

    if status.running {
        let pid = status
            .pid
            .map(|p| p.to_string())
            .unwrap_or_else(|| "?".into());
        let uptime = status
            .uptime
            .as_deref()
            .unwrap_or("unknown");

        lines.push(format!(
            "  Status:  {}  (PID {})",
            "running".green().bold(),
            pid
        ));
        lines.push(format!("  Uptime:  {}", uptime.cyan()));
    } else {
        lines.push(format!("  Status:  {}", "stopped".red().bold()));
    }

    lines.push(format!("  Log:     {}", status.log_file.dimmed()));

    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Config loading
// ---------------------------------------------------------------------------

/// Load daemon configuration from `~/.bfcode/config.json`.
///
/// Falls back to [`DaemonConfig::default()`] when the file is missing or the
/// `"daemon"` key is absent.
pub fn load_daemon_config() -> DaemonConfig {
    let path = match dirs::home_dir() {
        Some(h) => h.join(".bfcode/config.json"),
        None => return DaemonConfig::default(),
    };

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return DaemonConfig::default(),
    };

    let root: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return DaemonConfig::default(),
    };

    match root.get("daemon") {
        Some(val) => serde_json::from_value(val.clone()).unwrap_or_default(),
        None => DaemonConfig::default(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_humanize_duration() {
        assert_eq!(humanize_duration(0), "0s");
        assert_eq!(humanize_duration(42), "42s");
        assert_eq!(humanize_duration(90), "1m 30s");
        assert_eq!(humanize_duration(3661), "1h 1m");
        assert_eq!(humanize_duration(90_000), "1d 1h");
    }

    #[test]
    fn test_default_config() {
        let cfg = DaemonConfig::default();
        assert!(cfg.respawn);
        assert_eq!(cfg.max_respawns, 5);
        assert_eq!(cfg.auto_update_hours, 0);
        assert!(cfg.pid_file.ends_with("bfcode.pid"));
        assert!(cfg.log_file.ends_with("bfcode.log"));
    }

    #[test]
    fn test_config_roundtrip() {
        let cfg = DaemonConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let cfg2: DaemonConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg.pid_file, cfg2.pid_file);
        assert_eq!(cfg.respawn, cfg2.respawn);
    }

    #[test]
    fn test_format_status_running() {
        let status = DaemonStatus {
            running: true,
            pid: Some(12345),
            uptime: Some("2h 30m".into()),
            log_file: "/tmp/bfcode.log".into(),
            version: "0.6.0".into(),
        };
        let output = format_status(&status);
        assert!(output.contains("running"));
        assert!(output.contains("12345"));
    }

    #[test]
    fn test_format_status_stopped() {
        let status = DaemonStatus {
            running: false,
            pid: None,
            uptime: None,
            log_file: "/tmp/bfcode.log".into(),
            version: "0.6.0".into(),
        };
        let output = format_status(&status);
        assert!(output.contains("stopped"));
    }

    #[test]
    fn test_generate_systemd_unit() {
        let unit = generate_systemd_unit().unwrap();
        assert!(unit.contains("[Unit]"));
        assert!(unit.contains("gateway start"));
        assert!(unit.contains("Restart=on-failure"));
    }

    #[test]
    fn test_generate_launchd_plist() {
        let plist = generate_launchd_plist().unwrap();
        assert!(plist.contains("com.bfcode.daemon"));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("gateway"));
    }

    #[test]
    fn test_current_platform_target() {
        let target = current_platform_target();
        assert!(!target.is_empty());
        // Should contain an arch.
        assert!(target.starts_with("x86_64") || target.starts_with("aarch64") || target.starts_with("unknown"));
    }

    #[test]
    fn test_daemon_status_no_pid_file() {
        let cfg = DaemonConfig {
            pid_file: "/tmp/bfcode-test-nonexistent.pid".into(),
            ..Default::default()
        };
        let status = daemon_status(&cfg);
        assert!(!status.running);
        assert!(status.pid.is_none());
    }

    #[test]
    fn test_daemon_config_custom_values() {
        let cfg = DaemonConfig {
            pid_file: "/var/run/custom.pid".into(),
            log_file: "/var/log/custom.log".into(),
            respawn: false,
            max_respawns: 10,
            auto_update_hours: 24,
        };
        assert_eq!(cfg.pid_file, "/var/run/custom.pid");
        assert_eq!(cfg.log_file, "/var/log/custom.log");
        assert!(!cfg.respawn);
        assert_eq!(cfg.max_respawns, 10);
        assert_eq!(cfg.auto_update_hours, 24);

        // Roundtrip through JSON preserves custom values.
        let json = serde_json::to_string(&cfg).unwrap();
        let cfg2: DaemonConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg2.pid_file, "/var/run/custom.pid");
        assert_eq!(cfg2.log_file, "/var/log/custom.log");
        assert!(!cfg2.respawn);
        assert_eq!(cfg2.max_respawns, 10);
        assert_eq!(cfg2.auto_update_hours, 24);
    }

    #[test]
    fn test_daemon_status_not_running() {
        let status = DaemonStatus {
            running: false,
            pid: None,
            uptime: None,
            log_file: "/tmp/bfcode.log".into(),
            version: "1.0.0".into(),
        };
        assert!(!status.running);
        assert!(status.pid.is_none());
        assert!(status.uptime.is_none());
        assert_eq!(status.log_file, "/tmp/bfcode.log");
        assert_eq!(status.version, "1.0.0");
    }

    #[test]
    fn test_daemon_status_running() {
        let status = DaemonStatus {
            running: true,
            pid: Some(42),
            uptime: Some("3h 15m".into()),
            log_file: "/var/log/bfcode.log".into(),
            version: "2.0.0".into(),
        };
        assert!(status.running);
        assert_eq!(status.pid, Some(42));
        assert_eq!(status.uptime.as_deref(), Some("3h 15m"));
        assert_eq!(status.version, "2.0.0");
    }

    #[test]
    fn test_update_info_serialization() {
        let info = UpdateInfo {
            current_version: "1.0.0".into(),
            latest_version: "1.1.0".into(),
            download_url: "https://example.com/release/v1.1.0".into(),
            release_notes: "Bug fixes and improvements.".into(),
        };

        let json = serde_json::to_string(&info).unwrap();
        let info2: UpdateInfo = serde_json::from_str(&json).unwrap();

        assert_eq!(info2.current_version, "1.0.0");
        assert_eq!(info2.latest_version, "1.1.0");
        assert_eq!(info2.download_url, "https://example.com/release/v1.1.0");
        assert_eq!(info2.release_notes, "Bug fixes and improvements.");
    }

    #[test]
    fn test_format_status_stopped_details() {
        let status = DaemonStatus {
            running: false,
            pid: None,
            uptime: None,
            log_file: "/home/user/.bfcode/bfcode.log".into(),
            version: "0.8.0".into(),
        };
        let output = format_status(&status);
        assert!(output.contains("stopped"));
        assert!(output.contains("0.8.0"));
        assert!(output.contains("/home/user/.bfcode/bfcode.log"));
        // Should NOT contain PID or Uptime lines when stopped.
        assert!(!output.contains("PID"));
        assert!(!output.contains("Uptime"));
    }

    #[test]
    fn test_format_status_running_with_pid() {
        let status = DaemonStatus {
            running: true,
            pid: Some(1234),
            uptime: Some("5m 30s".into()),
            log_file: "/tmp/test.log".into(),
            version: "0.9.0".into(),
        };
        let output = format_status(&status);
        assert!(output.contains("running"));
        assert!(output.contains("1234"));
        assert!(output.contains("5m 30s"));
        assert!(output.contains("0.9.0"));
        assert!(output.contains("/tmp/test.log"));
    }

    #[test]
    fn test_daemon_status_function() {
        // Use a PID file path that definitely does not exist.
        let cfg = DaemonConfig {
            pid_file: "/tmp/bfcode-unit-test-no-such-file-98765.pid".into(),
            log_file: "/tmp/bfcode-unit-test.log".into(),
            respawn: true,
            max_respawns: 3,
            auto_update_hours: 0,
        };
        let status = daemon_status(&cfg);
        assert!(!status.running);
        assert!(status.pid.is_none());
        assert!(status.uptime.is_none());
        assert_eq!(status.log_file, "/tmp/bfcode-unit-test.log");
        assert_eq!(status.version, CURRENT_VERSION);
    }

    #[test]
    fn test_generate_systemd_unit_sections() {
        let unit = generate_systemd_unit().unwrap();
        // Verify all three required systemd sections are present.
        assert!(unit.contains("[Unit]"));
        assert!(unit.contains("[Service]"));
        assert!(unit.contains("[Install]"));
        // Verify key directives.
        assert!(unit.contains("Description=bfcode"));
        assert!(unit.contains("Type=simple"));
        assert!(unit.contains("ExecStart="));
        assert!(unit.contains("gateway start"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn test_generate_launchd_plist_xml() {
        let plist = generate_launchd_plist().unwrap();
        // Verify it is valid XML plist structure.
        assert!(plist.contains("<?xml version=\"1.0\""));
        assert!(plist.contains("<!DOCTYPE plist"));
        assert!(plist.contains("<plist version=\"1.0\">"));
        assert!(plist.contains("</plist>"));
        // Verify key elements.
        assert!(plist.contains("<key>Label</key>"));
        assert!(plist.contains("com.bfcode.daemon"));
        assert!(plist.contains("<key>ProgramArguments</key>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(plist.contains("<true/>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("<key>ThrottleInterval</key>"));
        assert!(plist.contains("gateway"));
        assert!(plist.contains("start"));
    }
}
