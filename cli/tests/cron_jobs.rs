//! Integration tests for cron job management.
//!
//! Tests the full lifecycle: add → schedule → output → remove, using JSONL persistence
//! and the `cron` crate for schedule parsing.
//!
//! Run:
//!   cargo test --test cron_jobs
//!   cargo test --test cron_jobs -- --ignored   # includes AI prompt test

use bfcode::cron::{CronManager, CronOutput, CronPromptRequest, JobKind};

/// Create a CronManager backed by a temp JSONL file.
fn isolated_manager() -> (CronManager, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let path = dir.path().join("cron.jsonl");
    let manager = CronManager::load_from(path);
    (manager, dir)
}

/// Reload from the same temp path.
fn reload_manager(dir: &tempfile::TempDir) -> CronManager {
    let path = dir.path().join("cron.jsonl");
    CronManager::load_from(path)
}

// ============================================================
// Schedule normalization (cron crate)
// ============================================================

#[test]
fn test_normalize_shorthand_to_cron_expr() {
    use bfcode::cron::normalize_schedule;
    assert_eq!(normalize_schedule("10s").unwrap(), "*/10 * * * * *");
    assert_eq!(normalize_schedule("5m").unwrap(), "0 */5 * * * *");
    assert_eq!(normalize_schedule("1h").unwrap(), "0 0 */1 * * *");
    assert_eq!(normalize_schedule("daily").unwrap(), "0 0 0 * * *");
    assert_eq!(normalize_schedule("hourly").unwrap(), "0 0 * * * *");
}

#[test]
fn test_raw_cron_expression_passthrough() {
    use bfcode::cron::normalize_schedule;
    // Standard 6-field cron expressions pass through
    let expr = normalize_schedule("*/5 * * * * *").unwrap();
    assert_eq!(expr, "*/5 * * * * *");
}

#[test]
fn test_invalid_schedule_rejected() {
    use bfcode::cron::normalize_schedule;
    assert!(normalize_schedule("abc").is_err());
    assert!(normalize_schedule("0s").is_err());
    assert!(normalize_schedule("5x").is_err());
}

// ============================================================
// JSONL persistence: add, list, remove
// ============================================================

#[test]
fn test_add_and_list_shell_job() {
    let (mut manager, _dir) = isolated_manager();

    let id = manager
        .add_job("10s", "date", "print date every 10s", JobKind::Shell)
        .expect("add_job failed");

    assert_eq!(id.len(), 8);

    let jobs = manager.list_jobs();
    assert_eq!(jobs.len(), 1);

    let job = &jobs[0];
    assert_eq!(job.schedule, "*/10 * * * * *"); // normalized to cron expr
    assert_eq!(job.command, "date");
    assert_eq!(job.description, "print date every 10s");
    assert!(job.enabled);
    assert_eq!(job.kind, JobKind::Shell);
}

#[test]
fn test_add_with_raw_cron_expression() {
    let (mut manager, _dir) = isolated_manager();

    let id = manager
        .add_job("*/30 * * * * *", "uptime", "every 30s via cron expr", JobKind::Shell)
        .expect("add_job failed");

    let job = manager.list_jobs().iter().find(|j| j.id == id).unwrap();
    assert_eq!(job.schedule, "*/30 * * * * *");
}

#[test]
fn test_add_and_list_prompt_job() {
    let (mut manager, _dir) = isolated_manager();

    let id = manager
        .add_job("5m", "summarize logs", "log summary", JobKind::Prompt)
        .expect("add_job failed");

    let job = manager.list_jobs().iter().find(|j| j.id == id).cloned().unwrap();
    assert_eq!(job.kind, JobKind::Prompt);
    assert_eq!(job.command, "summarize logs");
    assert_eq!(job.schedule, "0 */5 * * * *");
}

#[test]
fn test_remove_job() {
    let (mut manager, _dir) = isolated_manager();
    let id = manager.add_job("30s", "echo hi", "test", JobKind::Shell).unwrap();
    assert!(manager.remove_job(&id).unwrap());
    assert!(manager.list_jobs().is_empty());
}

#[test]
fn test_remove_nonexistent_returns_false() {
    let (mut manager, _dir) = isolated_manager();
    assert!(!manager.remove_job("nonexistent_id").unwrap());
}

#[test]
fn test_enable_disable_job() {
    let (mut manager, _dir) = isolated_manager();
    let id = manager.add_job("1h", "uptime", "check uptime", JobKind::Shell).unwrap();

    assert!(manager.set_enabled(&id, false).unwrap());
    assert!(!manager.list_jobs().iter().find(|j| j.id == id).unwrap().enabled);

    assert!(manager.set_enabled(&id, true).unwrap());
    assert!(manager.list_jobs().iter().find(|j| j.id == id).unwrap().enabled);
}

// ============================================================
// JSONL persistence round-trip
// ============================================================

#[test]
fn test_jsonl_persistence_round_trip() {
    let (mut manager, dir) = isolated_manager();

    let id1 = manager.add_job("10s", "date", "print date", JobKind::Shell).unwrap();
    let id2 = manager.add_job("1m", "uptime", "check uptime", JobKind::Shell).unwrap();

    let reloaded = reload_manager(&dir);
    let jobs = reloaded.list_jobs();
    assert_eq!(jobs.len(), 2);
    assert!(jobs.iter().any(|j| j.id == id1));
    assert!(jobs.iter().any(|j| j.id == id2));

    let j1 = jobs.iter().find(|j| j.id == id1).unwrap();
    assert_eq!(j1.schedule, "*/10 * * * * *");
    assert_eq!(j1.command, "date");
}

// ============================================================
// Scheduler: shell job
// ============================================================

#[tokio::test]
async fn test_scheduler_shell_date_job() {
    let (mut manager, dir) = isolated_manager();

    // 1s interval for fast test
    let id = manager.add_job("1s", "date", "print date every 10s", JobKind::Shell).unwrap();

    let (prompt_tx, _prompt_rx) = tokio::sync::mpsc::unbounded_channel::<CronPromptRequest>();
    let (output_tx, mut output_rx) = tokio::sync::broadcast::channel::<CronOutput>(16);

    let scheduler_manager = reload_manager(&dir);
    let handle = scheduler_manager.start_scheduler(prompt_tx, output_tx);

    let output = tokio::time::timeout(std::time::Duration::from_secs(5), output_rx.recv())
        .await
        .expect("timeout waiting for cron output")
        .expect("channel error");

    assert_eq!(output.job_id, id);
    assert_eq!(output.kind, "shell");
    assert_eq!(output.status, "ok");
    assert!(!output.stdout.as_deref().unwrap_or("").trim().is_empty());
    println!("[test] cron shell output: {}", output.stdout.as_deref().unwrap_or("").trim());

    handle.abort();
}

// ============================================================
// Scheduler: prompt job
// ============================================================

#[tokio::test]
async fn test_scheduler_prompt_job() {
    let (mut manager, dir) = isolated_manager();
    let id = manager.add_job("1s", "what time is it?", "time prompt", JobKind::Prompt).unwrap();

    let (prompt_tx, mut prompt_rx) = tokio::sync::mpsc::unbounded_channel::<CronPromptRequest>();
    let (output_tx, mut output_rx) = tokio::sync::broadcast::channel::<CronOutput>(16);

    let scheduler_manager = reload_manager(&dir);
    let handle = scheduler_manager.start_scheduler(prompt_tx, output_tx);

    let req = tokio::time::timeout(std::time::Duration::from_secs(5), prompt_rx.recv())
        .await
        .expect("timeout")
        .expect("channel closed");

    assert_eq!(req.job_id, id);
    assert_eq!(req.prompt, "what time is it?");

    let output = tokio::time::timeout(std::time::Duration::from_secs(2), output_rx.recv())
        .await
        .expect("timeout")
        .expect("channel error");

    assert_eq!(output.job_id, id);
    assert_eq!(output.kind, "prompt");
    assert_eq!(output.status, "queued");

    handle.abort();
}

// ============================================================
// Full lifecycle: add → run → remove
// ============================================================

#[tokio::test]
async fn test_full_lifecycle_add_run_remove() {
    let (mut manager, dir) = isolated_manager();

    let id = manager.add_job("1s", "echo hello_from_cron", "lifecycle test", JobKind::Shell).unwrap();
    assert_eq!(manager.list_jobs().len(), 1);

    let (prompt_tx, _) = tokio::sync::mpsc::unbounded_channel::<CronPromptRequest>();
    let (output_tx, mut output_rx) = tokio::sync::broadcast::channel::<CronOutput>(16);

    let scheduler_manager = reload_manager(&dir);
    let handle = scheduler_manager.start_scheduler(prompt_tx, output_tx);

    let output = tokio::time::timeout(std::time::Duration::from_secs(5), output_rx.recv())
        .await
        .expect("timeout")
        .expect("channel error");

    assert_eq!(output.job_id, id);
    assert_eq!(output.status, "ok");
    assert!(output.stdout.as_deref().unwrap_or("").contains("hello_from_cron"));

    handle.abort();

    let mut manager = reload_manager(&dir);
    assert!(manager.remove_job(&id).unwrap());
    assert!(manager.list_jobs().is_empty());

    let reloaded = reload_manager(&dir);
    assert!(reloaded.list_jobs().is_empty());
}

// ============================================================
// exec_cron_tool (AI tool interface)
// ============================================================

#[test]
fn test_exec_cron_tool_add_list_remove() {
    use bfcode::cron::exec_cron_tool;

    // Remember how many jobs existed before
    let before = exec_cron_tool(r#"{"action":"list"}"#).unwrap();
    let before_count = if before.contains("No cron jobs") {
        0
    } else {
        before.lines().count()
    };

    // Add
    let result = exec_cron_tool(r#"{"action":"add","schedule":"10s","command":"date","description":"print date every 10s","kind":"shell"}"#).unwrap();
    assert!(result.contains("Cron job added"));
    assert!(result.contains("*/10 * * * * *"));
    println!("[test] {result}");

    // Extract job ID from result
    let id = result.split("id=").nth(1).unwrap().split(',').next().unwrap().trim();

    // List — should contain our job
    let result = exec_cron_tool(r#"{"action":"list"}"#).unwrap();
    assert!(result.contains(id));
    assert!(result.contains("date"));
    println!("[test] list: {result}");

    // Remove our job
    let remove_args = format!(r#"{{"action":"remove","id":"{id}"}}"#);
    let result = exec_cron_tool(&remove_args).unwrap();
    assert!(result.contains("removed"));
    println!("[test] {result}");

    // Verify our job is gone (count should be back to before)
    let after = exec_cron_tool(r#"{"action":"list"}"#).unwrap();
    let after_count = if after.contains("No cron jobs") {
        0
    } else {
        after.lines().count()
    };
    assert_eq!(after_count, before_count, "job count should return to pre-test level");
}

// ============================================================
// AI prompt integration test
// ============================================================

/// Run `bfcode chat --oneshot "<prompt>"` and return the AI response.
async fn run_ai_prompt(prompt: &str) -> Result<String, String> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        tokio::process::Command::new("bfcode")
            .args(["chat", "--oneshot", prompt])
            .output(),
    )
    .await
    .map_err(|_| "AI prompt timed out after 120s".to_string())?
    .map_err(|e| format!("Failed to spawn bfcode: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        return Err(format!("bfcode exited with {}: {}", output.status, stderr.trim()));
    }

    // Combine stdout for the response
    Ok(if stdout.is_empty() { stderr } else { stdout })
}

/// End-to-end test:
///   1. AI adds a cron job ("add cron: print date every 10s")
///   2. Verify job was created in CronManager
///   3. Start scheduler, wait for actual execution, verify printed output
///   4. AI removes the cron job
///   5. Verify job is gone
///
/// Requires `bfcode` binary installed with cron tool support.
/// Run: make test-cron-ai
#[tokio::test]
#[ignore] // Requires `bfcode` binary with AI access; run with `--ignored`
async fn test_ai_prompt_add_run_verify_remove_cron() {
    // Record pre-existing job IDs so we only track what this test creates
    let pre_existing_ids: Vec<String> = CronManager::load()
        .list_jobs()
        .iter()
        .map(|j| j.id.clone())
        .collect();
    println!("[ai-test] Pre-existing cron jobs: {}", pre_existing_ids.len());

    // ── Step 1: Ask AI to add a cron job ──
    println!("\n[ai-test] === Step 1: Ask AI to add cron job ===");
    let response = run_ai_prompt("add cron: print date every 10s").await;
    match &response {
        Ok(r) => println!("[ai-test] AI response: {r}"),
        Err(e) => {
            println!("[ai-test] AI error: {e}");
            panic!("AI failed to add cron job");
        }
    }

    // ── Step 2: Verify the cron job was actually created ──
    println!("\n[ai-test] === Step 2: Verify cron job created ===");
    let manager = CronManager::load();
    let jobs = manager.list_jobs();
    let new_jobs: Vec<_> = jobs
        .iter()
        .filter(|j| !pre_existing_ids.contains(&j.id))
        .collect();
    println!("[ai-test] Total cron jobs: {}, new: {}", jobs.len(), new_jobs.len());
    for job in &new_jobs {
        println!(
            "[ai-test]   id={} schedule=\"{}\" cmd=\"{}\" desc=\"{}\" enabled={}",
            job.id, job.schedule, job.command, job.description, job.enabled
        );
    }
    assert!(!new_jobs.is_empty(), "AI should have created at least one new cron job");

    let job = new_jobs
        .iter()
        .find(|j| j.command.contains("date"))
        .expect("Should find a new cron job with 'date' command");
    let job_id = job.id.clone();
    let job_schedule = job.schedule.clone();
    println!("[ai-test] Found job: id={job_id}, schedule={job_schedule}");

    // ── Step 3: Start scheduler, verify the job actually runs ──
    println!("\n[ai-test] === Step 3: Start scheduler, verify job executes ===");
    let (prompt_tx, _prompt_rx) =
        tokio::sync::mpsc::unbounded_channel::<CronPromptRequest>();
    let (output_tx, mut output_rx) =
        tokio::sync::broadcast::channel::<CronOutput>(16);

    let scheduler_manager = CronManager::load();
    let handle = scheduler_manager.start_scheduler(prompt_tx, output_tx);

    // Wait for the cron job to fire and produce output
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        output_rx.recv(),
    )
    .await
    .expect("timeout: cron job did not fire within 15s")
    .expect("broadcast channel error");

    println!("[ai-test] Cron output received:");
    println!("[ai-test]   job_id:  {}", output.job_id);
    println!("[ai-test]   kind:    {}", output.kind);
    println!("[ai-test]   status:  {}", output.status);
    println!(
        "[ai-test]   stdout:  {}",
        output.stdout.as_deref().unwrap_or("(none)")
    );
    println!(
        "[ai-test]   stderr:  {}",
        output.stderr.as_deref().unwrap_or("(none)")
    );

    assert_eq!(output.job_id, job_id, "output should be from our job");
    assert_eq!(output.kind, "shell");
    assert_eq!(output.status, "ok", "job should succeed");
    let stdout = output.stdout.as_deref().unwrap_or("");
    assert!(
        !stdout.trim().is_empty(),
        "date command should print something, got empty stdout"
    );
    println!("[ai-test] Cron job printed: {}", stdout.trim());

    handle.abort();
    // Wait for scheduler to fully stop so it doesn't re-persist the job
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // ── Step 4: Ask AI to remove the cron job ──
    println!("\n[ai-test] === Step 4: Ask AI to remove cron job ===");
    let remove_prompt = format!("remove cron job {job_id}");
    println!("[ai-test] Prompt: {remove_prompt}");
    let response = run_ai_prompt(&remove_prompt).await;
    match &response {
        Ok(r) => println!("[ai-test] AI response: {r}"),
        Err(e) => {
            println!("[ai-test] AI error: {e}");
            panic!("AI failed to remove cron job");
        }
    }

    // ── Step 5: Verify removal and cleanup ──
    println!("\n[ai-test] === Step 5: Verify cron job removed ===");
    let mut manager = CronManager::load();
    let remaining: Vec<_> = manager
        .list_jobs()
        .iter()
        .filter(|j| j.id == job_id)
        .collect();
    println!("[ai-test] Jobs with id={job_id}: {}", remaining.len());
    assert!(
        remaining.is_empty(),
        "Cron job {job_id} should be removed, but still found"
    );
    println!("[ai-test] Cron job {job_id} successfully removed");

    // Cleanup: remove any other jobs created by this test
    let leftover_ids: Vec<String> = manager
        .list_jobs()
        .iter()
        .filter(|j| !pre_existing_ids.contains(&j.id))
        .map(|j| j.id.clone())
        .collect();
    for id in &leftover_ids {
        let _ = manager.remove_job(id);
        println!("[ai-test] Cleaned up leftover job: {id}");
    }

    println!("\n[ai-test] === ALL STEPS PASSED ===");
}

// ============================================================
// Scheduler: removing a job from disk stops it from firing
// ============================================================

#[tokio::test]
async fn test_scheduler_stops_after_disk_removal() {
    let (mut manager, dir) = isolated_manager();

    // Add a 1s job
    let id = manager
        .add_job("1s", "echo still_running", "removal test", JobKind::Shell)
        .unwrap();

    let (prompt_tx, _) = tokio::sync::mpsc::unbounded_channel::<CronPromptRequest>();
    let (output_tx, mut output_rx) = tokio::sync::broadcast::channel::<CronOutput>(32);

    let scheduler_manager = reload_manager(&dir);
    let handle = scheduler_manager.start_scheduler(prompt_tx, output_tx);

    // Wait for at least one execution
    let output = tokio::time::timeout(std::time::Duration::from_secs(5), output_rx.recv())
        .await
        .expect("timeout waiting for first execution")
        .expect("channel error");
    assert_eq!(output.job_id, id);
    assert_eq!(output.status, "ok");
    println!("[test] Job fired once: {}", output.stdout.as_deref().unwrap_or("").trim());

    // Now remove the job from disk (simulating `exec_cron_tool remove`)
    let mut mgr = reload_manager(&dir);
    assert!(mgr.remove_job(&id).unwrap());
    assert!(mgr.list_jobs().is_empty());
    println!("[test] Job removed from disk");

    // Wait 3 seconds — the scheduler should NOT fire the job again
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), output_rx.recv()).await;
    match result {
        Err(_) => {
            // Timeout — no output received, which is correct!
            println!("[test] PASS: scheduler did not fire removed job");
        }
        Ok(Ok(output)) => {
            panic!(
                "BUG: scheduler fired removed job {}! output: {:?}",
                output.job_id, output.stdout
            );
        }
        Ok(Err(e)) => {
            // Channel lagged or closed — acceptable
            println!("[test] Channel error (acceptable): {e}");
        }
    }

    handle.abort();
}

// ============================================================
// Format display
// ============================================================

#[test]
fn test_format_jobs_empty() {
    let (manager, _dir) = isolated_manager();
    let display = manager.format_jobs();
    assert!(display.contains("No cron jobs"));
}

#[test]
fn test_format_jobs_with_entries() {
    let (mut manager, _dir) = isolated_manager();
    let id = manager.add_job("10s", "date", "test format", JobKind::Shell).unwrap();

    let display = manager.format_jobs();
    assert!(display.contains(&id));
    assert!(display.contains("*/10 * * * * *"));
}
