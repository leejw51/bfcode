//! Shared test utilities — single CWD lock to prevent cross-module races.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Global lock for ALL tests that change the working directory.
/// CWD is process-global, so a single lock across modules is required.
pub static CWD_LOCK: Mutex<()> = Mutex::new(());

/// Run a closure with cwd temporarily set to `dir`, holding the global CWD_LOCK.
pub fn with_cwd<F, R>(dir: &Path, f: F) -> R
where
    F: FnOnce() -> R,
{
    let _lock = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let original = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::env::set_current_dir(&original).unwrap();
    match result {
        Ok(r) => r,
        Err(e) => std::panic::resume_unwind(e),
    }
}

/// Create a temp directory for tests. Caller should clean up.
pub fn tmp_dir(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "bfcode_test_{name}_{}_{}",
        std::process::id(),
        format!("{:?}", std::thread::current().id())
            .replace("ThreadId(", "")
            .replace(")", "")
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}
