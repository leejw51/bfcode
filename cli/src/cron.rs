use anyhow::{Context, Result};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write as IoWrite};
use std::path::PathBuf;
use std::str::FromStr;

/// Job type: shell command or AI prompt
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum JobKind {
    /// Execute a shell command via `sh -c`
    Shell,
    /// Send a prompt to the AI model for processing
    Prompt,
}

impl Default for JobKind {
    fn default() -> Self {
        JobKind::Shell
    }
}

/// A scheduled cron job
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub id: String,
    /// Cron expression (e.g. "*/10 * * * * *") or shorthand ("10s", "5m", "1h", "daily")
    pub schedule: String,
    /// Shell command or AI prompt to execute
    pub command: String,
    /// Description of what this job does
    pub description: String,
    /// Whether this job is enabled
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Job type: "shell" or "prompt"
    #[serde(default)]
    pub kind: JobKind,
    /// Last run timestamp (ISO 8601)
    #[serde(default)]
    pub last_run: Option<String>,
    /// Last run status
    #[serde(default)]
    pub last_status: Option<String>,
    /// Consecutive error count
    #[serde(default)]
    pub error_count: u32,
    /// Created timestamp
    pub created_at: String,
}

fn default_true() -> bool {
    true
}

/// Convert a shorthand schedule string to a cron expression, or validate
/// a raw cron expression.
///
/// Shorthands:
///   "10s"   → every 10 seconds  → "*/10 * * * * *"
///   "5m"    → every 5 minutes   → "0 */5 * * * *"
///   "1h"    → every hour        → "0 0 */1 * * *"
///   "daily" → once a day        → "0 0 0 * * *"
///   "hourly"→ once an hour      → "0 0 * * * *"
///
/// If the input already looks like a cron expression (contains spaces), it is
/// parsed with the `cron` crate and returned as-is.
pub fn normalize_schedule(s: &str) -> Result<String> {
    let s = s.trim();

    // If it contains spaces, treat as a raw cron expression
    if s.contains(' ') {
        // Validate with the cron crate
        cron::Schedule::from_str(s)
            .map_err(|e| anyhow::anyhow!("Invalid cron expression: {e}"))?;
        return Ok(s.to_string());
    }

    let lower = s.to_lowercase();
    let expr = match lower.as_str() {
        "daily" => "0 0 0 * * *".to_string(),
        "hourly" => "0 0 * * * *".to_string(),
        _ => {
            if lower.len() < 2 {
                anyhow::bail!(
                    "Invalid schedule: {s:?}. Use cron expression (\"*/10 * * * * *\"), \
                     shorthand (\"10s\", \"5m\", \"1h\"), or keyword (\"daily\", \"hourly\")."
                );
            }
            let (digits, suffix) = lower.split_at(lower.len() - 1);
            let value: u64 = digits
                .parse()
                .with_context(|| format!("Invalid number in schedule: {digits:?}"))?;
            if value == 0 {
                anyhow::bail!("Schedule interval must be > 0");
            }
            match suffix {
                "s" => format!("*/{value} * * * * *"),
                "m" => format!("0 */{value} * * * *"),
                "h" => format!("0 0 */{value} * * *"),
                _ => anyhow::bail!(
                    "Unknown suffix {suffix:?}. Use 's' (seconds), 'm' (minutes), or 'h' (hours), \
                     or a full cron expression."
                ),
            }
        }
    };

    // Validate the generated expression
    cron::Schedule::from_str(&expr)
        .map_err(|e| anyhow::anyhow!("Generated invalid cron expression {expr:?}: {e}"))?;

    Ok(expr)
}

/// Check whether a job is due based on its cron schedule and last_run timestamp.
pub fn is_job_due(schedule_expr: &str, last_run: Option<&str>) -> bool {
    let schedule = match cron::Schedule::from_str(schedule_expr) {
        Ok(s) => s,
        Err(_) => return false,
    };

    let now = chrono::Utc::now();

    match last_run {
        Some(last) => {
            if let Ok(last_dt) = chrono::DateTime::parse_from_rfc3339(last) {
                let last_utc = last_dt.with_timezone(&chrono::Utc);
                // Get next scheduled time after last_run — if it's <= now, job is due
                schedule
                    .after(&last_utc)
                    .next()
                    .map(|next| next <= now)
                    .unwrap_or(false)
            } else {
                true // unparseable last_run → run immediately
            }
        }
        None => true, // never run → run immediately
    }
}

/// Output from a cron job execution, for broadcasting to terminal and gateway.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronOutput {
    pub job_id: String,
    pub kind: String,
    pub description: String,
    pub command: String,
    pub status: String,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub timestamp: String,
}

/// Return the path to `.bfcode/cron.jsonl` inside the user's home directory.
fn cron_file_path() -> PathBuf {
    let base = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join(".bfcode").join("cron.jsonl")
}

/// Generate a short random hex ID (8 characters).
fn generate_id() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let state = RandomState::new();
    let mut hasher = state.build_hasher();
    hasher.write_u64(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64,
    );
    format!("{:08x}", hasher.finish() as u32)
}

/// Cron job manager
pub struct CronManager {
    jobs: Vec<CronJob>,
    /// Custom file path for persistence (None = default ~/.bfcode/cron.jsonl).
    custom_path: Option<PathBuf>,
}

impl CronManager {
    /// Load jobs from `.bfcode/cron.jsonl` (one JSON object per line).
    pub fn load() -> Self {
        Self::load_from(cron_file_path())
    }

    /// Load jobs from a custom JSONL path (useful for testing).
    pub fn load_from(path: PathBuf) -> Self {
        let jobs = if path.exists() {
            match std::fs::File::open(&path) {
                Ok(file) => {
                    let reader = std::io::BufReader::new(file);
                    reader
                        .lines()
                        .filter_map(|line| {
                            let line = line.ok()?;
                            let trimmed = line.trim();
                            if trimmed.is_empty() {
                                return None;
                            }
                            serde_json::from_str::<CronJob>(trimmed).ok()
                        })
                        .collect()
                }
                Err(_) => Vec::new(),
            }
        } else {
            Vec::new()
        };
        Self {
            jobs,
            custom_path: Some(path),
        }
    }

    /// Save jobs to `.bfcode/cron.jsonl` (one JSON object per line).
    pub fn save(&self) -> Result<()> {
        let path = self.custom_path.clone().unwrap_or_else(cron_file_path);
        write_jsonl_to(&self.jobs, &path)
    }

    /// Add a new job, returns the job ID.
    ///
    /// `schedule` can be a shorthand ("10s", "5m", "1h", "daily", "hourly")
    /// or a standard 6-field cron expression ("*/10 * * * * *").
    pub fn add_job(
        &mut self,
        schedule: &str,
        command: &str,
        description: &str,
        kind: JobKind,
    ) -> Result<String> {
        let cron_expr = normalize_schedule(schedule)?;

        let id = generate_id();
        let now = chrono::Utc::now().to_rfc3339();
        let job = CronJob {
            id: id.clone(),
            schedule: cron_expr,
            command: command.to_string(),
            description: description.to_string(),
            enabled: true,
            kind,
            last_run: None,
            last_status: None,
            error_count: 0,
            created_at: now,
        };
        self.jobs.push(job);
        self.save()?;
        Ok(id)
    }

    /// Remove a job by ID. Returns `true` if the job was found and removed.
    pub fn remove_job(&mut self, id: &str) -> Result<bool> {
        let before = self.jobs.len();
        self.jobs.retain(|j| j.id != id);
        let removed = self.jobs.len() < before;
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    /// Enable or disable a job. Returns `true` if the job was found.
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<bool> {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
            job.enabled = enabled;
            self.save()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// List all jobs.
    pub fn list_jobs(&self) -> &[CronJob] {
        &self.jobs
    }

    /// Format jobs for display.
    pub fn format_jobs(&self) -> String {
        if self.jobs.is_empty() {
            return "No cron jobs configured.".dimmed().to_string();
        }

        let mut lines = Vec::new();
        lines.push(format!(
            "{:<10} {:<8} {:<20} {:<8} {:<24} {}",
            "ID".bold(),
            "Kind".bold(),
            "Schedule".bold(),
            "Status".bold(),
            "Description".bold(),
            "Command/Prompt".bold(),
        ));
        lines.push("-".repeat(106));

        for job in &self.jobs {
            let status = if job.enabled {
                "on".green().to_string()
            } else {
                "off".red().to_string()
            };
            let kind_str = match job.kind {
                JobKind::Shell => "shell",
                JobKind::Prompt => "prompt",
            };
            let cmd_display = if job.command.len() > 40 {
                format!("{}...", &job.command[..37])
            } else {
                job.command.clone()
            };
            lines.push(format!(
                "{:<10} {:<8} {:<20} {:<8} {:<24} {}",
                job.id.cyan(),
                kind_str.yellow(),
                job.schedule,
                status,
                job.description,
                cmd_display.dimmed(),
            ));
        }

        lines.join("\n")
    }

    /// Start the background scheduler.
    ///
    /// Uses the `cron` crate to determine when each job is due.
    /// Shell jobs execute via `sh -c`. Prompt jobs are sent to the provided
    /// `prompt_sender` channel for the main loop to process.
    ///
    /// `output_sender` broadcasts job output to terminal and gateway websocket users.
    pub fn start_scheduler(
        self,
        prompt_sender: tokio::sync::mpsc::UnboundedSender<CronPromptRequest>,
        output_sender: tokio::sync::broadcast::Sender<CronOutput>,
    ) -> tokio::task::JoinHandle<()> {
        let persist_path = self.custom_path.clone().unwrap_or_else(cron_file_path);
        tokio::spawn(async move {
            let mut jobs = self.jobs;

            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;

                // Reload jobs from disk so that external add/remove/enable/disable
                // changes are picked up by the running scheduler.
                let fresh = CronManager::load_from(persist_path.clone());
                // Preserve in-memory last_run timestamps for jobs that still exist
                for fj in &fresh.jobs {
                    if let Some(existing) = jobs.iter().find(|j| j.id == fj.id) {
                        // keep the in-memory last_run if the file doesn't have a newer one
                        let _ = existing; // used below
                    }
                }
                let mut merged: Vec<CronJob> = fresh.jobs;
                for mj in merged.iter_mut() {
                    if let Some(existing) = jobs.iter().find(|j| j.id == mj.id) {
                        // Prefer the in-memory last_run (most up-to-date from this tick loop)
                        if mj.last_run.is_none() && existing.last_run.is_some() {
                            mj.last_run = existing.last_run.clone();
                        }
                    }
                }
                jobs = merged;

                for i in 0..jobs.len() {
                    if !jobs[i].enabled {
                        continue;
                    }

                    if !is_job_due(&jobs[i].schedule, jobs[i].last_run.as_deref()) {
                        continue;
                    }

                    let kind_label = match jobs[i].kind {
                        JobKind::Shell => "shell",
                        JobKind::Prompt => "prompt",
                    };
                    eprintln!(
                        "{} running {} job {} ({}): {}",
                        "[cron]".yellow(),
                        kind_label.cyan(),
                        jobs[i].id.cyan(),
                        jobs[i].description.dimmed(),
                        jobs[i].command.dimmed(),
                    );

                    let timestamp = chrono::Utc::now().to_rfc3339();
                    jobs[i].last_run = Some(timestamp.clone());

                    match jobs[i].kind {
                        JobKind::Shell => {
                            let result = tokio::process::Command::new("sh")
                                .arg("-c")
                                .arg(&jobs[i].command)
                                .output()
                                .await;

                            match result {
                                Ok(output) if output.status.success() => {
                                    jobs[i].last_status = Some("ok".into());
                                    jobs[i].error_count = 0;
                                    let stdout_str =
                                        String::from_utf8_lossy(&output.stdout).to_string();
                                    let stderr_str =
                                        String::from_utf8_lossy(&output.stderr).to_string();
                                    eprintln!(
                                        "{} job {} {}",
                                        "[cron]".yellow(),
                                        jobs[i].id.cyan(),
                                        "completed successfully".green(),
                                    );
                                    if !stdout_str.trim().is_empty() {
                                        eprintln!(
                                            "{} output: {}",
                                            "[cron]".yellow(),
                                            stdout_str.trim(),
                                        );
                                    }
                                    let _ = output_sender.send(CronOutput {
                                        job_id: jobs[i].id.clone(),
                                        kind: "shell".into(),
                                        description: jobs[i].description.clone(),
                                        command: jobs[i].command.clone(),
                                        status: "ok".into(),
                                        stdout: Some(stdout_str),
                                        stderr: if stderr_str.trim().is_empty() {
                                            None
                                        } else {
                                            Some(stderr_str)
                                        },
                                        timestamp: timestamp.clone(),
                                    });
                                }
                                Ok(output) => {
                                    jobs[i].error_count += 1;
                                    let code = output
                                        .status
                                        .code()
                                        .map(|c| c.to_string())
                                        .unwrap_or_else(|| "unknown".into());
                                    let stderr_str =
                                        String::from_utf8_lossy(&output.stderr).to_string();
                                    let stdout_str =
                                        String::from_utf8_lossy(&output.stdout).to_string();
                                    jobs[i].last_status = Some(format!("error(exit {code})"));
                                    eprintln!(
                                        "{} job {} {} (exit {}): {}",
                                        "[cron]".yellow(),
                                        jobs[i].id.cyan(),
                                        "failed".red(),
                                        code,
                                        stderr_str.trim().dimmed(),
                                    );
                                    let _ = output_sender.send(CronOutput {
                                        job_id: jobs[i].id.clone(),
                                        kind: "shell".into(),
                                        description: jobs[i].description.clone(),
                                        command: jobs[i].command.clone(),
                                        status: format!("error(exit {code})"),
                                        stdout: if stdout_str.trim().is_empty() {
                                            None
                                        } else {
                                            Some(stdout_str)
                                        },
                                        stderr: Some(stderr_str),
                                        timestamp: timestamp.clone(),
                                    });
                                }
                                Err(e) => {
                                    jobs[i].error_count += 1;
                                    jobs[i].last_status = Some(format!("error: {e}"));
                                    eprintln!(
                                        "{} job {} {}: {}",
                                        "[cron]".yellow(),
                                        jobs[i].id.cyan(),
                                        "execution error".red(),
                                        e.to_string().dimmed(),
                                    );
                                    let _ = output_sender.send(CronOutput {
                                        job_id: jobs[i].id.clone(),
                                        kind: "shell".into(),
                                        description: jobs[i].description.clone(),
                                        command: jobs[i].command.clone(),
                                        status: format!("error: {e}"),
                                        stdout: None,
                                        stderr: Some(e.to_string()),
                                        timestamp: timestamp.clone(),
                                    });
                                }
                            }
                        }
                        JobKind::Prompt => {
                            let req = CronPromptRequest {
                                job_id: jobs[i].id.clone(),
                                prompt: jobs[i].command.clone(),
                            };
                            if prompt_sender.send(req).is_ok() {
                                jobs[i].last_status = Some("ok".into());
                                jobs[i].error_count = 0;
                                eprintln!(
                                    "{} prompt job {} {}",
                                    "[cron]".yellow(),
                                    jobs[i].id.cyan(),
                                    "queued for processing".green(),
                                );
                                let _ = output_sender.send(CronOutput {
                                    job_id: jobs[i].id.clone(),
                                    kind: "prompt".into(),
                                    description: jobs[i].description.clone(),
                                    command: jobs[i].command.clone(),
                                    status: "queued".into(),
                                    stdout: None,
                                    stderr: None,
                                    timestamp: timestamp.clone(),
                                });
                            } else {
                                jobs[i].error_count += 1;
                                jobs[i].last_status = Some("error: channel closed".into());
                                eprintln!(
                                    "{} prompt job {} {}: channel closed",
                                    "[cron]".yellow(),
                                    jobs[i].id.cyan(),
                                    "failed".red(),
                                );
                                let _ = output_sender.send(CronOutput {
                                    job_id: jobs[i].id.clone(),
                                    kind: "prompt".into(),
                                    description: jobs[i].description.clone(),
                                    command: jobs[i].command.clone(),
                                    status: "error: channel closed".into(),
                                    stdout: None,
                                    stderr: None,
                                    timestamp: timestamp.clone(),
                                });
                            }
                        }
                    }

                    let _ = write_jsonl_to(&jobs, &persist_path);
                }
            }
        })
    }
}

/// A prompt request from a cron job to be processed by the main loop.
#[derive(Debug)]
pub struct CronPromptRequest {
    pub job_id: String,
    pub prompt: String,
}

/// Execute the `cron` tool call from the AI.
///
/// Supports actions: "add", "remove", "list", "enable", "disable".
pub fn exec_cron_tool(arguments: &str) -> Result<String> {
    #[derive(Deserialize)]
    struct CronArgs {
        action: String,
        #[serde(default)]
        schedule: Option<String>,
        #[serde(default)]
        command: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        kind: Option<String>,
        #[serde(default)]
        id: Option<String>,
    }

    let args: CronArgs = serde_json::from_str(arguments)
        .context("Invalid cron tool arguments")?;

    let mut manager = CronManager::load();

    match args.action.as_str() {
        "add" => {
            let schedule = args.schedule.as_deref()
                .ok_or_else(|| anyhow::anyhow!("Missing 'schedule' for cron add"))?;
            let command = args.command.as_deref()
                .ok_or_else(|| anyhow::anyhow!("Missing 'command' for cron add"))?;
            let description = args.description.as_deref().unwrap_or("");
            let kind = match args.kind.as_deref() {
                Some("prompt") => JobKind::Prompt,
                _ => JobKind::Shell,
            };
            let id = manager.add_job(schedule, command, description, kind.clone())?;
            let job = manager.list_jobs().iter().find(|j| j.id == id).unwrap();
            Ok(format!(
                "Cron job added: id={}, schedule=\"{}\", kind={}, command=\"{}\"",
                id,
                job.schedule,
                if kind == JobKind::Shell { "shell" } else { "prompt" },
                command
            ))
        }
        "remove" => {
            let id = args.id.as_deref()
                .ok_or_else(|| anyhow::anyhow!("Missing 'id' for cron remove"))?;
            if manager.remove_job(id)? {
                Ok(format!("Cron job {id} removed."))
            } else {
                Ok(format!("Cron job '{id}' not found."))
            }
        }
        "list" => {
            let jobs = manager.list_jobs();
            if jobs.is_empty() {
                Ok("No cron jobs configured.".to_string())
            } else {
                let mut lines = Vec::new();
                for job in jobs {
                    let kind = match job.kind {
                        JobKind::Shell => "shell",
                        JobKind::Prompt => "prompt",
                    };
                    lines.push(format!(
                        "id={} kind={} schedule=\"{}\" enabled={} desc=\"{}\" cmd=\"{}\"",
                        job.id, kind, job.schedule, job.enabled, job.description, job.command
                    ));
                }
                Ok(lines.join("\n"))
            }
        }
        "enable" => {
            let id = args.id.as_deref()
                .ok_or_else(|| anyhow::anyhow!("Missing 'id' for cron enable"))?;
            if manager.set_enabled(id, true)? {
                Ok(format!("Cron job {id} enabled."))
            } else {
                Ok(format!("Cron job '{id}' not found."))
            }
        }
        "disable" => {
            let id = args.id.as_deref()
                .ok_or_else(|| anyhow::anyhow!("Missing 'id' for cron disable"))?;
            if manager.set_enabled(id, false)? {
                Ok(format!("Cron job {id} disabled."))
            } else {
                Ok(format!("Cron job '{id}' not found."))
            }
        }
        _ => anyhow::bail!("Unknown cron action: {}. Use add/remove/list/enable/disable.", args.action),
    }
}

/// Write jobs as JSONL (one JSON object per line) to the given path.
fn write_jsonl_to(jobs: &[CronJob], path: &std::path::Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(path)
        .with_context(|| format!("Failed to create {}", path.display()))?;
    let mut writer = std::io::BufWriter::new(file);
    for job in jobs {
        let line = serde_json::to_string(job).context("Failed to serialize cron job")?;
        writeln!(writer, "{}", line)
            .with_context(|| format!("Failed to write to {}", path.display()))?;
    }
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_seconds() {
        let expr = normalize_schedule("10s").unwrap();
        assert_eq!(expr, "*/10 * * * * *");
    }

    #[test]
    fn test_normalize_minutes() {
        let expr = normalize_schedule("5m").unwrap();
        assert_eq!(expr, "0 */5 * * * *");
    }

    #[test]
    fn test_normalize_hours() {
        let expr = normalize_schedule("1h").unwrap();
        assert_eq!(expr, "0 0 */1 * * *");
    }

    #[test]
    fn test_normalize_keywords() {
        assert_eq!(normalize_schedule("daily").unwrap(), "0 0 0 * * *");
        assert_eq!(normalize_schedule("hourly").unwrap(), "0 0 * * * *");
        assert_eq!(normalize_schedule("Daily").unwrap(), "0 0 0 * * *");
    }

    #[test]
    fn test_normalize_raw_cron_expression() {
        let expr = normalize_schedule("*/5 * * * * *").unwrap();
        assert_eq!(expr, "*/5 * * * * *");
    }

    #[test]
    fn test_normalize_invalid() {
        assert!(normalize_schedule("abc").is_err());
        assert!(normalize_schedule("0s").is_err());
        assert!(normalize_schedule("5x").is_err());
    }

    #[test]
    fn test_is_job_due_never_run() {
        assert!(is_job_due("*/10 * * * * *", None));
    }

    #[test]
    fn test_is_job_due_recently_run() {
        let now = chrono::Utc::now().to_rfc3339();
        // Just ran — should NOT be due for a 1-hour schedule
        assert!(!is_job_due("0 0 * * * *", Some(&now)));
    }

    #[test]
    fn test_is_job_due_old_run() {
        let old = (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
        // Ran 2h ago with hourly schedule — should be due
        assert!(is_job_due("0 0 * * * *", Some(&old)));
    }

    #[test]
    fn test_generate_id_length() {
        let id = generate_id();
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
