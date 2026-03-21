use anyhow::{Context, Result};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A scheduled cron job
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub id: String,
    /// Human-readable schedule (e.g., "5m", "1h", "30s", "daily")
    pub schedule: String,
    /// Shell command to execute
    pub command: String,
    /// Description of what this job does
    pub description: String,
    /// Whether this job is enabled
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Last run timestamp (ISO 8601)
    #[serde(default)]
    pub last_run: Option<String>,
    /// Created timestamp
    pub created_at: String,
}

fn default_true() -> bool {
    true
}

/// Parse a schedule string into seconds.
///
/// Supports: "30s", "5m", "1h", "daily" (24h), "hourly" (1h)
pub fn parse_schedule(s: &str) -> Result<u64> {
    let s = s.trim().to_lowercase();
    match s.as_str() {
        "daily" => return Ok(86400),
        "hourly" => return Ok(3600),
        _ => {}
    }

    if s.len() < 2 {
        anyhow::bail!(
            "Invalid schedule string: {s:?}. Use e.g. \"30s\", \"5m\", \"1h\", \"daily\", \"hourly\"."
        );
    }

    let (digits, suffix) = s.split_at(s.len() - 1);
    let value: u64 = digits
        .parse()
        .with_context(|| format!("Invalid numeric value in schedule: {digits:?}"))?;

    if value == 0 {
        anyhow::bail!("Schedule interval must be greater than zero");
    }

    let seconds = match suffix {
        "s" => value,
        "m" => value * 60,
        "h" => value * 3600,
        _ => anyhow::bail!(
            "Unknown schedule suffix {suffix:?}. Use 's' (seconds), 'm' (minutes), or 'h' (hours)."
        ),
    };

    Ok(seconds)
}

/// Return the path to `.bfcode/cron.json` inside the user's home directory.
fn cron_file_path() -> PathBuf {
    let base = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join(".bfcode").join("cron.json")
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
}

impl CronManager {
    /// Load jobs from `.bfcode/cron.json`.
    /// Returns an empty manager if the file does not exist or cannot be parsed.
    pub fn load() -> Self {
        let path = cron_file_path();
        let jobs = if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(contents) => serde_json::from_str::<Vec<CronJob>>(&contents).unwrap_or_default(),
                Err(_) => Vec::new(),
            }
        } else {
            Vec::new()
        };
        Self { jobs }
    }

    /// Save jobs to `.bfcode/cron.json`.
    pub fn save(&self) -> Result<()> {
        let path = cron_file_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        }
        let json =
            serde_json::to_string_pretty(&self.jobs).context("Failed to serialize cron jobs")?;
        std::fs::write(&path, json)
            .with_context(|| format!("Failed to write {}", path.display()))?;
        Ok(())
    }

    /// Add a new job, returns the job ID.
    pub fn add_job(&mut self, schedule: &str, command: &str, description: &str) -> Result<String> {
        // Validate the schedule before adding
        parse_schedule(schedule)?;

        let id = generate_id();
        let now = chrono::Utc::now().to_rfc3339();
        let job = CronJob {
            id: id.clone(),
            schedule: schedule.to_string(),
            command: command.to_string(),
            description: description.to_string(),
            enabled: true,
            last_run: None,
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
            "{:<10} {:<10} {:<8} {:<30} {}",
            "ID".bold(),
            "Schedule".bold(),
            "Status".bold(),
            "Description".bold(),
            "Command".bold(),
        ));
        lines.push("-".repeat(90));

        for job in &self.jobs {
            let status = if job.enabled {
                "on".green().to_string()
            } else {
                "off".red().to_string()
            };
            lines.push(format!(
                "{:<10} {:<10} {:<8} {:<30} {}",
                job.id.cyan(),
                job.schedule,
                status,
                job.description,
                job.command.dimmed(),
            ));
        }

        lines.join("\n")
    }

    /// Start the background scheduler.
    ///
    /// Runs enabled jobs at their configured intervals by checking each job's
    /// `last_run` timestamp. Executes commands via `sh -c`. Continues running
    /// even when individual jobs fail.
    ///
    /// Returns a `JoinHandle` that can be aborted on application exit.
    pub fn start_scheduler(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            // We keep our own mutable copy of the jobs list so we can update
            // `last_run` in memory. We also persist changes back to disk.
            let mut jobs = self.jobs;

            loop {
                // Sleep a short tick between evaluation rounds.
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;

                let now = chrono::Utc::now();

                for i in 0..jobs.len() {
                    if !jobs[i].enabled {
                        continue;
                    }

                    let interval_secs = match parse_schedule(&jobs[i].schedule) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!(
                                "{} {} {}",
                                "[cron]".yellow(),
                                format!("bad schedule for job {}: {e}", jobs[i].id).red(),
                                "- skipping".dimmed(),
                            );
                            continue;
                        }
                    };

                    let is_due = match &jobs[i].last_run {
                        Some(last) => {
                            if let Ok(last_dt) = chrono::DateTime::parse_from_rfc3339(last) {
                                let elapsed =
                                    now.signed_duration_since(last_dt).num_seconds().max(0) as u64;
                                elapsed >= interval_secs
                            } else {
                                // Unparseable last_run — treat as due
                                true
                            }
                        }
                        None => true,
                    };

                    if !is_due {
                        continue;
                    }

                    eprintln!(
                        "{} running job {} ({}): {}",
                        "[cron]".yellow(),
                        jobs[i].id.cyan(),
                        jobs[i].description.dimmed(),
                        jobs[i].command.dimmed(),
                    );

                    let result = tokio::process::Command::new("sh")
                        .arg("-c")
                        .arg(&jobs[i].command)
                        .output()
                        .await;

                    let timestamp = chrono::Utc::now().to_rfc3339();
                    jobs[i].last_run = Some(timestamp);

                    match result {
                        Ok(output) => {
                            if output.status.success() {
                                eprintln!(
                                    "{} job {} {}",
                                    "[cron]".yellow(),
                                    jobs[i].id.cyan(),
                                    "completed successfully".green(),
                                );
                            } else {
                                let code = output
                                    .status
                                    .code()
                                    .map(|c| c.to_string())
                                    .unwrap_or_else(|| "unknown".into());
                                let stderr = String::from_utf8_lossy(&output.stderr);
                                eprintln!(
                                    "{} job {} {} (exit {}): {}",
                                    "[cron]".yellow(),
                                    jobs[i].id.cyan(),
                                    "failed".red(),
                                    code,
                                    stderr.trim().dimmed(),
                                );
                            }
                        }
                        Err(e) => {
                            eprintln!(
                                "{} job {} {}: {}",
                                "[cron]".yellow(),
                                jobs[i].id.cyan(),
                                "execution error".red(),
                                e.to_string().dimmed(),
                            );
                        }
                    }

                    // Persist updated last_run to disk (best-effort).
                    let _ = persist_jobs(&jobs);
                }
            }
        })
    }
}

/// Helper to persist the jobs list to disk from within the scheduler loop.
fn persist_jobs(jobs: &[CronJob]) -> Result<()> {
    let path = cron_file_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(jobs)?;
    std::fs::write(&path, json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_schedule_seconds() {
        assert_eq!(parse_schedule("30s").unwrap(), 30);
        assert_eq!(parse_schedule("1s").unwrap(), 1);
    }

    #[test]
    fn test_parse_schedule_minutes() {
        assert_eq!(parse_schedule("5m").unwrap(), 300);
        assert_eq!(parse_schedule("1m").unwrap(), 60);
    }

    #[test]
    fn test_parse_schedule_hours() {
        assert_eq!(parse_schedule("1h").unwrap(), 3600);
        assert_eq!(parse_schedule("2h").unwrap(), 7200);
    }

    #[test]
    fn test_parse_schedule_keywords() {
        assert_eq!(parse_schedule("daily").unwrap(), 86400);
        assert_eq!(parse_schedule("hourly").unwrap(), 3600);
        assert_eq!(parse_schedule("Daily").unwrap(), 86400);
    }

    #[test]
    fn test_parse_schedule_invalid() {
        assert!(parse_schedule("abc").is_err());
        assert!(parse_schedule("0s").is_err());
        assert!(parse_schedule("5x").is_err());
    }

    #[test]
    fn test_generate_id_length() {
        let id = generate_id();
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
