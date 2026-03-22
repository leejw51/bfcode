use anyhow::{Context, Result, bail};
use colored::Colorize;
use git2::{Repository, Signature};
use std::path::{Path, PathBuf};

/// Information about a git worktree
pub struct WorktreeInfo {
    pub name: String,
    pub path: PathBuf,
    pub branch: Option<String>,
    pub is_main: bool,
}

/// Manager for git worktree operations using libgit2
pub struct WorktreeManager {
    repo: Repository,
}

impl WorktreeManager {
    /// Open the repository at or above the current directory
    pub fn open() -> Result<Self> {
        let repo = Repository::discover(".").context("Not inside a git repository")?;
        Ok(Self { repo })
    }

    /// Open the repository at a specific path
    pub fn open_at(path: &Path) -> Result<Self> {
        let repo = Repository::discover(path)
            .with_context(|| format!("Not a git repository: {}", path.display()))?;
        Ok(Self { repo })
    }

    /// List all worktrees
    pub fn list(&self) -> Result<Vec<WorktreeInfo>> {
        let mut worktrees = Vec::new();

        // Add the main worktree
        let workdir = self
            .repo
            .workdir()
            .context("Bare repository has no working directory")?;
        let head_branch = self
            .repo
            .head()
            .ok()
            .and_then(|h| h.shorthand().map(String::from));
        worktrees.push(WorktreeInfo {
            name: "(main)".to_string(),
            path: workdir.to_path_buf(),
            branch: head_branch,
            is_main: true,
        });

        // List linked worktrees
        let wt_names = self.repo.worktrees()?;
        for name in wt_names.iter() {
            let Some(name) = name else { continue };
            if let Ok(wt) = self.repo.find_worktree(name) {
                let wt_path = wt.path().to_path_buf();
                // Try to detect the branch by opening the worktree repo
                let branch = Repository::open(&wt_path).ok().and_then(|r| {
                    let head = r.head().ok()?;
                    head.shorthand().map(String::from)
                });
                worktrees.push(WorktreeInfo {
                    name: name.to_string(),
                    path: wt_path,
                    branch,
                    is_main: false,
                });
            }
        }

        Ok(worktrees)
    }

    /// Create a new worktree
    /// If `branch` is Some, checks out that branch; otherwise creates a new branch from HEAD.
    pub fn create(&self, name: &str, path: &Path, branch: Option<&str>) -> Result<PathBuf> {
        // Ensure path doesn't already exist
        if path.exists() {
            bail!("Path already exists: {}", path.display());
        }

        // Check if worktree name already exists
        let existing = self.repo.worktrees()?;
        for existing_name in existing.iter() {
            if existing_name == Some(name) {
                bail!("Worktree '{}' already exists", name);
            }
        }

        let reference = if let Some(branch_name) = branch {
            // Find existing branch
            self.repo
                .find_branch(branch_name, git2::BranchType::Local)
                .with_context(|| format!("Branch '{}' not found", branch_name))?
                .into_reference()
        } else {
            // Create a new branch from HEAD
            let head = self.repo.head().context("Cannot get HEAD")?;
            let commit = head.peel_to_commit().context("HEAD is not a commit")?;
            self.repo
                .branch(name, &commit, false)
                .with_context(|| format!("Failed to create branch '{}'", name))?
                .into_reference()
        };

        self.repo
            .worktree(
                name,
                path,
                Some(&git2::WorktreeAddOptions::new().reference(Some(&reference))),
            )
            .with_context(|| format!("Failed to create worktree at {}", path.display()))?;

        Ok(path.to_path_buf())
    }

    /// Remove a worktree by name
    pub fn remove(&self, name: &str, force: bool) -> Result<()> {
        let wt = self
            .repo
            .find_worktree(name)
            .with_context(|| format!("Worktree '{}' not found", name))?;

        // Check if worktree is valid (not locked, no changes)
        if !force {
            if wt
                .is_locked()
                .ok()
                .map_or(false, |s| matches!(s, git2::WorktreeLockStatus::Locked(_)))
            {
                bail!(
                    "Worktree '{}' is locked. Use --force to remove anyway.",
                    name
                );
            }
            // Check for uncommitted changes by opening the worktree repo
            let wt_path = wt.path();
            if let Ok(wt_repo) = Repository::open(wt_path) {
                let statuses = wt_repo.statuses(None)?;
                if !statuses.is_empty() {
                    bail!(
                        "Worktree '{}' has {} uncommitted change(s). Use --force to remove anyway.",
                        name,
                        statuses.len()
                    );
                }
            }
        }

        // Remove the worktree directory
        let wt_path = wt.path().to_path_buf();
        if wt_path.exists() {
            std::fs::remove_dir_all(&wt_path)
                .with_context(|| format!("Failed to remove directory: {}", wt_path.display()))?;
        }

        // Prune the worktree reference
        wt.prune(Some(
            &mut git2::WorktreePruneOptions::new()
                .valid(true)
                .working_tree(true),
        ))
        .with_context(|| format!("Failed to prune worktree '{}'", name))?;

        Ok(())
    }

    /// Reset a worktree to match its branch HEAD (discard all changes)
    pub fn reset(&self, name: &str) -> Result<()> {
        let wt = self
            .repo
            .find_worktree(name)
            .with_context(|| format!("Worktree '{}' not found", name))?;

        let wt_path = wt.path();
        let wt_repo = Repository::open(wt_path)
            .with_context(|| format!("Cannot open worktree repo at {}", wt_path.display()))?;

        let head = wt_repo.head().context("Cannot get worktree HEAD")?;
        let obj = head
            .peel(git2::ObjectType::Commit)
            .context("HEAD is not a commit")?;

        wt_repo
            .reset(&obj, git2::ResetType::Hard, None)
            .context("Failed to reset worktree")?;

        Ok(())
    }

    /// Lock a worktree to prevent accidental removal
    pub fn lock(&self, name: &str, reason: Option<&str>) -> Result<()> {
        let wt = self
            .repo
            .find_worktree(name)
            .with_context(|| format!("Worktree '{}' not found", name))?;

        if wt
            .is_locked()
            .ok()
            .map_or(false, |s| matches!(s, git2::WorktreeLockStatus::Locked(_)))
        {
            bail!("Worktree '{}' is already locked", name);
        }

        wt.lock(reason).context("Failed to lock worktree")?;
        Ok(())
    }

    /// Unlock a worktree
    pub fn unlock(&self, name: &str) -> Result<()> {
        let wt = self
            .repo
            .find_worktree(name)
            .with_context(|| format!("Worktree '{}' not found", name))?;

        if !wt
            .is_locked()
            .ok()
            .map_or(false, |s| matches!(s, git2::WorktreeLockStatus::Locked(_)))
        {
            bail!("Worktree '{}' is not locked", name);
        }

        wt.unlock().context("Failed to unlock worktree")?;
        Ok(())
    }
}

/// Format worktree list for display
pub fn format_worktrees(worktrees: &[WorktreeInfo]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{}\n",
        format!("Git Worktrees ({})", worktrees.len()).bold()
    ));
    out.push_str(&"─".repeat(60));
    out.push('\n');

    for wt in worktrees {
        let marker = if wt.is_main { "●" } else { "○" };
        let branch_str = wt
            .branch
            .as_deref()
            .unwrap_or("(detached)")
            .cyan()
            .to_string();
        let name_str = if wt.is_main {
            wt.name.bold().to_string()
        } else {
            wt.name.green().to_string()
        };

        out.push_str(&format!(
            "  {} {} [{}]\n    {}\n",
            marker,
            name_str,
            branch_str,
            wt.path.display().to_string().dimmed()
        ));
    }

    out
}

/// Helper to initialize a test repo with an initial commit
fn init_test_repo(path: &Path) -> Result<Repository> {
    std::fs::create_dir_all(path)?;
    let repo = Repository::init(path)?;
    // Create an initial commit so HEAD exists
    let sig = Signature::now("Test", "test@test.com")?;
    let tree_id = repo.index()?.write_tree()?;
    {
        let tree = repo.find_tree(tree_id)?;
        repo.commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[])?;
    }
    Ok(repo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils;

    fn setup_repo() -> (PathBuf, Repository) {
        let dir = test_utils::tmp_dir("worktree");
        let repo = init_test_repo(&dir).expect("failed to init test repo");
        (dir, repo)
    }

    #[test]
    fn test_open_at_valid_repo() {
        let (dir, _repo) = setup_repo();
        test_utils::with_cwd(&dir, || {
            let mgr = WorktreeManager::open();
            assert!(mgr.is_ok());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_open_at_not_a_repo() {
        let dir = test_utils::tmp_dir("worktree_norepo");
        let mgr = WorktreeManager::open_at(&dir);
        assert!(mgr.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_list_main_worktree() {
        let (dir, _repo) = setup_repo();
        test_utils::with_cwd(&dir, || {
            let mgr = WorktreeManager::open().unwrap();
            let wts = mgr.list().unwrap();
            assert_eq!(wts.len(), 1);
            assert!(wts[0].is_main);
            assert_eq!(wts[0].name, "(main)");
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_create_and_list_worktree() {
        let (dir, _repo) = setup_repo();
        let wt_path = dir.join("wt-feature");
        test_utils::with_cwd(&dir, || {
            let mgr = WorktreeManager::open().unwrap();
            let created = mgr.create("feature", &wt_path, None);
            assert!(created.is_ok(), "create failed: {:?}", created.err());

            let wts = mgr.list().unwrap();
            assert_eq!(wts.len(), 2);

            let linked = wts.iter().find(|w| !w.is_main).unwrap();
            assert_eq!(linked.name, "feature");
            assert_eq!(linked.branch.as_deref(), Some("feature"));
        });
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&wt_path);
    }

    #[test]
    fn test_create_worktree_duplicate_name() {
        let (dir, _repo) = setup_repo();
        let wt_path1 = dir.join("wt-dup1");
        let wt_path2 = dir.join("wt-dup2");
        test_utils::with_cwd(&dir, || {
            let mgr = WorktreeManager::open().unwrap();
            mgr.create("dupname", &wt_path1, None).unwrap();
            let result = mgr.create("dupname", &wt_path2, None);
            assert!(result.is_err());
            assert!(
                result.unwrap_err().to_string().contains("already exists"),
                "expected 'already exists' error"
            );
        });
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&wt_path1);
    }

    #[test]
    fn test_create_worktree_with_existing_branch() {
        let (dir, repo) = setup_repo();
        // Create a branch
        let head = repo.head().unwrap();
        let commit = head.peel_to_commit().unwrap();
        repo.branch("my-branch", &commit, false).unwrap();

        let wt_path = dir.join("wt-mybranch");
        test_utils::with_cwd(&dir, || {
            let mgr = WorktreeManager::open().unwrap();
            let result = mgr.create("mybranch-wt", &wt_path, Some("my-branch"));
            assert!(
                result.is_ok(),
                "create with branch failed: {:?}",
                result.err()
            );

            let wts = mgr.list().unwrap();
            let linked = wts.iter().find(|w| w.name == "mybranch-wt").unwrap();
            assert_eq!(linked.branch.as_deref(), Some("my-branch"));
        });
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&wt_path);
    }

    #[test]
    fn test_create_worktree_nonexistent_branch() {
        let (dir, _repo) = setup_repo();
        let wt_path = dir.join("wt-noexist");
        test_utils::with_cwd(&dir, || {
            let mgr = WorktreeManager::open().unwrap();
            let result = mgr.create("noexist-wt", &wt_path, Some("no-such-branch"));
            assert!(result.is_err());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_remove_worktree() {
        let (dir, _repo) = setup_repo();
        let wt_path = dir.join("wt-removeme");
        test_utils::with_cwd(&dir, || {
            let mgr = WorktreeManager::open().unwrap();
            mgr.create("removeme", &wt_path, None).unwrap();
            assert!(wt_path.exists());

            mgr.remove("removeme", false).unwrap();
            assert!(!wt_path.exists());

            let wts = mgr.list().unwrap();
            assert_eq!(wts.len(), 1);
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_remove_worktree_with_changes_requires_force() {
        let (dir, _repo) = setup_repo();
        let wt_path = dir.join("wt-dirty");
        test_utils::with_cwd(&dir, || {
            let mgr = WorktreeManager::open().unwrap();
            mgr.create("dirty", &wt_path, None).unwrap();

            // Make a dirty change in the worktree
            std::fs::write(wt_path.join("dirty.txt"), "uncommitted").unwrap();

            let result = mgr.remove("dirty", false);
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("uncommitted"));

            // Force should work
            mgr.remove("dirty", true).unwrap();
            assert!(!wt_path.exists());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_remove_nonexistent_worktree() {
        let (dir, _repo) = setup_repo();
        test_utils::with_cwd(&dir, || {
            let mgr = WorktreeManager::open().unwrap();
            let result = mgr.remove("nonexistent", false);
            assert!(result.is_err());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_reset_worktree() {
        let (dir, _repo) = setup_repo();
        let wt_path = dir.join("wt-reset");
        test_utils::with_cwd(&dir, || {
            let mgr = WorktreeManager::open().unwrap();
            mgr.create("resetme", &wt_path, None).unwrap();

            // Create a file in worktree
            let file_path = wt_path.join("extra.txt");
            std::fs::write(&file_path, "should be removed after reset").unwrap();
            // Stage it
            let wt_repo = Repository::open(&wt_path).unwrap();
            let mut index = wt_repo.index().unwrap();
            index.add_path(Path::new("extra.txt")).unwrap();
            index.write().unwrap();

            // Reset should discard changes
            mgr.reset("resetme").unwrap();

            // After hard reset, the staged file should not be in the index
            let wt_repo2 = Repository::open(&wt_path).unwrap();
            let statuses = wt_repo2.statuses(None).unwrap();
            // The file may still exist on disk (untracked) after reset, but should not be staged
            let staged: Vec<_> = statuses
                .iter()
                .filter(|e| {
                    e.status().intersects(
                        git2::Status::INDEX_NEW
                            | git2::Status::INDEX_MODIFIED
                            | git2::Status::INDEX_DELETED,
                    )
                })
                .collect();
            assert!(staged.is_empty(), "expected no staged changes after reset");
        });
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&wt_path);
    }

    #[test]
    fn test_lock_and_unlock_worktree() {
        let (dir, _repo) = setup_repo();
        let wt_path = dir.join("wt-locktest");
        test_utils::with_cwd(&dir, || {
            let mgr = WorktreeManager::open().unwrap();
            mgr.create("locktest", &wt_path, None).unwrap();

            // Lock
            mgr.lock("locktest", Some("testing")).unwrap();

            // Double lock should fail
            let result = mgr.lock("locktest", None);
            assert!(result.is_err());

            // Unlock
            mgr.unlock("locktest").unwrap();

            // Double unlock should fail
            let result = mgr.unlock("locktest");
            assert!(result.is_err());
        });
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&wt_path);
    }

    #[test]
    fn test_format_worktrees_output() {
        let worktrees = vec![
            WorktreeInfo {
                name: "(main)".to_string(),
                path: PathBuf::from("/tmp/repo"),
                branch: Some("main".to_string()),
                is_main: true,
            },
            WorktreeInfo {
                name: "feature-x".to_string(),
                path: PathBuf::from("/tmp/repo-feature-x"),
                branch: Some("feature-x".to_string()),
                is_main: false,
            },
        ];

        let output = format_worktrees(&worktrees);
        assert!(output.contains("Git Worktrees (2)"));
        assert!(output.contains("(main)"));
        assert!(output.contains("feature-x"));
        assert!(output.contains("/tmp/repo"));
    }

    #[test]
    fn test_format_worktrees_detached() {
        let worktrees = vec![WorktreeInfo {
            name: "detached-wt".to_string(),
            path: PathBuf::from("/tmp/detached"),
            branch: None,
            is_main: false,
        }];

        let output = format_worktrees(&worktrees);
        assert!(output.contains("(detached)"));
    }

    #[test]
    fn test_format_worktrees_empty() {
        let output = format_worktrees(&[]);
        assert!(output.contains("Git Worktrees (0)"));
    }
}
