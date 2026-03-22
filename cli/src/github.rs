use anyhow::{Context, Result, bail};
use colored::Colorize;
use octocrab::Octocrab;
use std::fmt;

/// Parsed owner/repo from a git remote URL or "owner/repo" string
#[derive(Clone, Debug)]
pub struct RepoSlug {
    pub owner: String,
    pub repo: String,
}

impl fmt::Display for RepoSlug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.repo)
    }
}

impl RepoSlug {
    /// Parse from "owner/repo" or a GitHub URL
    pub fn parse(input: &str) -> Result<Self> {
        // Try SSH URL: git@github.com:owner/repo.git
        if input.starts_with("git@github.com:") {
            let path = input.trim_start_matches("git@github.com:");
            if let Some((owner, repo)) = path.split_once('/') {
                return Ok(Self {
                    owner: owner.to_string(),
                    repo: repo.trim_end_matches(".git").to_string(),
                });
            }
        }

        // Try HTTPS URL: https://github.com/owner/repo
        if input.contains("github.com/") {
            let after = input.split("github.com/").nth(1).unwrap_or("");
            let parts: Vec<&str> = after.splitn(3, '/').collect();
            if parts.len() >= 2 {
                return Ok(Self {
                    owner: parts[0].to_string(),
                    repo: parts[1].trim_end_matches(".git").to_string(),
                });
            }
        }

        // Try simple owner/repo format (must not contain ':' to avoid matching SSH URLs)
        if !input.contains(':') {
            if let Some((owner, repo)) = input.split_once('/') {
                if !owner.is_empty() && !repo.is_empty() && !repo.contains('/') {
                    return Ok(Self {
                        owner: owner.to_string(),
                        repo: repo.trim_end_matches(".git").to_string(),
                    });
                }
            }
        }

        bail!(
            "Cannot parse repository: '{}'. Expected owner/repo format.",
            input
        )
    }
}

/// Detect the repo slug from the current git repository's origin remote
pub fn detect_repo_slug() -> Result<RepoSlug> {
    let repo = git2::Repository::discover(".").context("Not inside a git repository")?;
    let remote = repo
        .find_remote("origin")
        .context("No 'origin' remote found")?;
    let url = remote.url().context("Origin remote has no URL")?;
    RepoSlug::parse(url)
}

/// Build an Octocrab client using GITHUB_TOKEN from env
fn build_client() -> Result<Octocrab> {
    let token = std::env::var("GITHUB_TOKEN")
        .or_else(|_| std::env::var("GH_TOKEN"))
        .context("GITHUB_TOKEN or GH_TOKEN environment variable is required")?;
    Octocrab::builder()
        .personal_token(token)
        .build()
        .context("Failed to build GitHub client")
}

/// PR summary for display
pub struct PrSummary {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub author: String,
    pub branch: String,
    pub created_at: String,
    pub url: String,
    pub draft: bool,
}

/// Issue summary for display
pub struct IssueSummary {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub author: String,
    pub labels: Vec<String>,
    pub created_at: String,
    pub url: String,
}

/// List pull requests
pub async fn list_prs(slug: &RepoSlug, state: Option<&str>) -> Result<Vec<PrSummary>> {
    let client = build_client()?;
    let mut page = client
        .pulls(&slug.owner, &slug.repo)
        .list()
        .state(match state {
            Some("closed") => octocrab::params::State::Closed,
            Some("all") => octocrab::params::State::All,
            _ => octocrab::params::State::Open,
        })
        .per_page(30)
        .send()
        .await
        .context("Failed to list pull requests")?;

    let prs = page.take_items();
    Ok(prs
        .into_iter()
        .map(|pr| PrSummary {
            number: pr.number,
            title: pr.title.unwrap_or_default(),
            state: pr
                .state
                .map(|s| format!("{:?}", s).to_lowercase())
                .unwrap_or_else(|| "unknown".into()),
            author: pr.user.map(|u| u.login).unwrap_or_else(|| "unknown".into()),
            branch: pr.head.ref_field,
            created_at: pr
                .created_at
                .map(|t| t.format("%Y-%m-%d").to_string())
                .unwrap_or_default(),
            url: pr.html_url.map(|u| u.to_string()).unwrap_or_default(),
            draft: pr.draft.unwrap_or(false),
        })
        .collect())
}

/// Get a single PR by number
pub async fn get_pr(slug: &RepoSlug, number: u64) -> Result<PrSummary> {
    let client = build_client()?;
    let pr = client
        .pulls(&slug.owner, &slug.repo)
        .get(number)
        .await
        .with_context(|| format!("Failed to get PR #{}", number))?;

    Ok(PrSummary {
        number: pr.number,
        title: pr.title.unwrap_or_default(),
        state: pr
            .state
            .map(|s| format!("{:?}", s).to_lowercase())
            .unwrap_or_else(|| "unknown".into()),
        author: pr.user.map(|u| u.login).unwrap_or_else(|| "unknown".into()),
        branch: pr.head.ref_field,
        created_at: pr
            .created_at
            .map(|t| t.format("%Y-%m-%d").to_string())
            .unwrap_or_default(),
        url: pr.html_url.map(|u| u.to_string()).unwrap_or_default(),
        draft: pr.draft.unwrap_or(false),
    })
}

/// Create a new pull request
pub async fn create_pr(
    slug: &RepoSlug,
    title: &str,
    head: &str,
    base: &str,
    body: Option<&str>,
    draft: bool,
) -> Result<PrSummary> {
    let client = build_client()?;
    let pulls = client.pulls(&slug.owner, &slug.repo);
    let mut builder = pulls.create(title, head, base);

    if let Some(body_text) = body {
        builder = builder.body(body_text);
    }
    if draft {
        builder = builder.draft(Some(draft));
    }

    let pr = builder
        .send()
        .await
        .context("Failed to create pull request")?;

    Ok(PrSummary {
        number: pr.number,
        title: pr.title.unwrap_or_default(),
        state: pr
            .state
            .map(|s| format!("{:?}", s).to_lowercase())
            .unwrap_or_else(|| "unknown".into()),
        author: pr.user.map(|u| u.login).unwrap_or_else(|| "unknown".into()),
        branch: pr.head.ref_field,
        created_at: pr
            .created_at
            .map(|t| t.format("%Y-%m-%d").to_string())
            .unwrap_or_default(),
        url: pr.html_url.map(|u| u.to_string()).unwrap_or_default(),
        draft: pr.draft.unwrap_or(false),
    })
}

/// List issues
pub async fn list_issues(slug: &RepoSlug, state: Option<&str>) -> Result<Vec<IssueSummary>> {
    let client = build_client()?;
    let mut page = client
        .issues(&slug.owner, &slug.repo)
        .list()
        .state(match state {
            Some("closed") => octocrab::params::State::Closed,
            Some("all") => octocrab::params::State::All,
            _ => octocrab::params::State::Open,
        })
        .per_page(30)
        .send()
        .await
        .context("Failed to list issues")?;

    let issues = page.take_items();
    Ok(issues
        .into_iter()
        .filter(|i| i.pull_request.is_none()) // Exclude PRs from issues list
        .map(|i| IssueSummary {
            number: i.number,
            title: i.title,
            state: format!("{:?}", i.state).to_lowercase(),
            author: i.user.login,
            labels: i.labels.iter().map(|l| l.name.clone()).collect(),
            created_at: i.created_at.format("%Y-%m-%d").to_string(),
            url: i.html_url.to_string(),
        })
        .collect())
}

/// Checkout a PR branch locally
pub async fn checkout_pr(slug: &RepoSlug, number: u64) -> Result<String> {
    let pr = get_pr(slug, number).await?;
    let branch = &pr.branch;

    // Use git2 to fetch and checkout
    let repo = git2::Repository::discover(".").context("Not inside a git repository")?;

    // Fetch the PR branch from origin
    let mut remote = repo
        .find_remote("origin")
        .context("No 'origin' remote found")?;

    let refspec = format!(
        "refs/pull/{}/head:refs/remotes/origin/pr-{}",
        number, number
    );
    remote
        .fetch(&[&refspec], None, None)
        .with_context(|| format!("Failed to fetch PR #{}", number))?;

    // Create local branch from the fetched ref
    let local_branch_name = format!("pr-{}", number);
    let fetch_ref = format!("refs/remotes/origin/pr-{}", number);
    let reference = repo
        .find_reference(&fetch_ref)
        .context("Failed to find fetched PR ref")?;
    let commit = reference
        .peel_to_commit()
        .context("PR ref is not a commit")?;

    // Create or reset local branch
    repo.branch(&local_branch_name, &commit, true)
        .context("Failed to create local branch for PR")?;

    // Checkout the branch
    let refname = format!("refs/heads/{}", local_branch_name);
    let obj = repo
        .revparse_single(&refname)
        .context("Failed to find branch")?;
    repo.checkout_tree(&obj, None)
        .context("Failed to checkout PR branch")?;
    repo.set_head(&refname)
        .context("Failed to set HEAD to PR branch")?;

    Ok(format!(
        "Checked out PR #{} ({}) → branch '{}'",
        number, branch, local_branch_name
    ))
}

/// Format PRs for display
pub fn format_prs(prs: &[PrSummary]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{}\n",
        format!("Pull Requests ({})", prs.len()).bold()
    ));
    out.push_str(&"─".repeat(70));
    out.push('\n');

    for pr in prs {
        let state_colored = match pr.state.as_str() {
            "open" => "open".green().to_string(),
            "closed" => "closed".red().to_string(),
            _ => pr.state.clone(),
        };
        let draft_marker = if pr.draft {
            " [draft]".dimmed().to_string()
        } else {
            String::new()
        };
        out.push_str(&format!(
            "  #{} {} {}{}\n    {} by {} on {} → {}\n",
            pr.number.to_string().cyan(),
            state_colored,
            pr.title.bold(),
            draft_marker,
            pr.branch.yellow(),
            pr.author.dimmed(),
            pr.created_at.dimmed(),
            pr.url.dimmed(),
        ));
    }

    if prs.is_empty() {
        out.push_str(&"  No pull requests found.\n".dimmed().to_string());
    }

    out
}

/// Format issues for display
pub fn format_issues(issues: &[IssueSummary]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{}\n",
        format!("Issues ({})", issues.len()).bold()
    ));
    out.push_str(&"─".repeat(70));
    out.push('\n');

    for issue in issues {
        let state_colored = match issue.state.as_str() {
            "open" => "open".green().to_string(),
            "closed" => "closed".red().to_string(),
            _ => issue.state.clone(),
        };
        let labels_str = if issue.labels.is_empty() {
            String::new()
        } else {
            format!(" [{}]", issue.labels.join(", "))
                .dimmed()
                .to_string()
        };
        out.push_str(&format!(
            "  #{} {} {}{}\n    by {} on {} → {}\n",
            issue.number.to_string().cyan(),
            state_colored,
            issue.title.bold(),
            labels_str,
            issue.author.dimmed(),
            issue.created_at.dimmed(),
            issue.url.dimmed(),
        ));
    }

    if issues.is_empty() {
        out.push_str(&"  No issues found.\n".dimmed().to_string());
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- RepoSlug parsing tests ---

    #[test]
    fn test_parse_owner_repo() {
        let slug = RepoSlug::parse("octocat/hello-world").unwrap();
        assert_eq!(slug.owner, "octocat");
        assert_eq!(slug.repo, "hello-world");
    }

    #[test]
    fn test_parse_owner_repo_with_git_suffix() {
        let slug = RepoSlug::parse("octocat/hello-world.git").unwrap();
        assert_eq!(slug.owner, "octocat");
        assert_eq!(slug.repo, "hello-world");
    }

    #[test]
    fn test_parse_ssh_url() {
        let slug = RepoSlug::parse("git@github.com:octocat/hello-world.git").unwrap();
        assert_eq!(slug.owner, "octocat");
        assert_eq!(slug.repo, "hello-world");
    }

    #[test]
    fn test_parse_ssh_url_no_git_suffix() {
        let slug = RepoSlug::parse("git@github.com:rust-lang/rust").unwrap();
        assert_eq!(slug.owner, "rust-lang");
        assert_eq!(slug.repo, "rust");
    }

    #[test]
    fn test_parse_https_url() {
        let slug = RepoSlug::parse("https://github.com/octocat/hello-world").unwrap();
        assert_eq!(slug.owner, "octocat");
        assert_eq!(slug.repo, "hello-world");
    }

    #[test]
    fn test_parse_https_url_with_git_suffix() {
        let slug = RepoSlug::parse("https://github.com/octocat/hello-world.git").unwrap();
        assert_eq!(slug.owner, "octocat");
        assert_eq!(slug.repo, "hello-world");
    }

    #[test]
    fn test_parse_https_url_with_trailing_path() {
        let slug = RepoSlug::parse("https://github.com/octocat/hello-world/tree/main").unwrap();
        assert_eq!(slug.owner, "octocat");
        assert_eq!(slug.repo, "hello-world");
    }

    #[test]
    fn test_parse_invalid_empty() {
        assert!(RepoSlug::parse("").is_err());
    }

    #[test]
    fn test_parse_invalid_no_slash() {
        assert!(RepoSlug::parse("just-a-name").is_err());
    }

    #[test]
    fn test_parse_invalid_empty_parts() {
        assert!(RepoSlug::parse("/repo").is_err());
        assert!(RepoSlug::parse("owner/").is_err());
    }

    #[test]
    fn test_display() {
        let slug = RepoSlug {
            owner: "octocat".into(),
            repo: "hello-world".into(),
        };
        assert_eq!(format!("{}", slug), "octocat/hello-world");
    }

    // --- detect_repo_slug tests ---

    #[test]
    fn test_detect_repo_slug_from_current_repo() {
        // This test runs inside the bfcode repo, so it should detect the origin
        let result = detect_repo_slug();
        // It may or may not succeed depending on whether 'origin' is set,
        // but it should not panic
        if let Ok(slug) = result {
            assert!(!slug.owner.is_empty());
            assert!(!slug.repo.is_empty());
        }
    }

    // --- Format tests ---

    /// Strip ANSI escape codes so assertions work regardless of color settings
    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // Skip until 'm' (end of ANSI SGR sequence)
                for inner in chars.by_ref() {
                    if inner == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn test_format_prs_empty() {
        let output = strip_ansi(&format_prs(&[]));
        assert!(output.contains("Pull Requests (0)"));
        assert!(output.contains("No pull requests found"));
    }

    #[test]
    fn test_format_prs_with_entries() {
        let prs = vec![
            PrSummary {
                number: 42,
                title: "Fix bug".into(),
                state: "open".into(),
                author: "alice".into(),
                branch: "fix/bug-42".into(),
                created_at: "2025-01-15".into(),
                url: "https://github.com/test/repo/pull/42".into(),
                draft: false,
            },
            PrSummary {
                number: 43,
                title: "WIP: New feature".into(),
                state: "open".into(),
                author: "bob".into(),
                branch: "feat/new".into(),
                created_at: "2025-01-16".into(),
                url: "https://github.com/test/repo/pull/43".into(),
                draft: true,
            },
        ];

        let output = strip_ansi(&format_prs(&prs));
        assert!(output.contains("Pull Requests (2)"));
        assert!(output.contains("Fix bug"));
        assert!(output.contains("#42"));
        assert!(output.contains("alice"));
        assert!(output.contains("fix/bug-42"));
        assert!(output.contains("[draft]"));
        assert!(output.contains("WIP: New feature"));
    }

    #[test]
    fn test_format_prs_closed_state() {
        let prs = vec![PrSummary {
            number: 1,
            title: "Closed PR".into(),
            state: "closed".into(),
            author: "dev".into(),
            branch: "old-branch".into(),
            created_at: "2024-06-01".into(),
            url: "https://github.com/t/r/pull/1".into(),
            draft: false,
        }];

        let output = strip_ansi(&format_prs(&prs));
        assert!(output.contains("closed"));
        assert!(output.contains("Closed PR"));
    }

    #[test]
    fn test_format_issues_empty() {
        let output = strip_ansi(&format_issues(&[]));
        assert!(output.contains("Issues (0)"));
        assert!(output.contains("No issues found"));
    }

    #[test]
    fn test_format_issues_with_entries() {
        let issues = vec![IssueSummary {
            number: 100,
            title: "Something broken".into(),
            state: "open".into(),
            author: "reporter".into(),
            labels: vec!["bug".into(), "urgent".into()],
            created_at: "2025-03-01".into(),
            url: "https://github.com/test/repo/issues/100".into(),
        }];

        let output = strip_ansi(&format_issues(&issues));
        assert!(output.contains("Issues (1)"));
        assert!(output.contains("Something broken"));
        assert!(output.contains("#100"));
        assert!(output.contains("reporter"));
        assert!(output.contains("bug"));
        assert!(output.contains("urgent"));
    }

    #[test]
    fn test_format_issues_no_labels() {
        let issues = vec![IssueSummary {
            number: 5,
            title: "Question".into(),
            state: "open".into(),
            author: "user".into(),
            labels: vec![],
            created_at: "2025-02-01".into(),
            url: "https://github.com/t/r/issues/5".into(),
        }];

        let output = strip_ansi(&format_issues(&issues));
        assert!(output.contains("Question"));
        // Should not contain label brackets
        assert!(!output.contains("["));
    }

    // --- build_client tests ---

    #[test]
    fn test_build_client_without_token() {
        // If neither GITHUB_TOKEN nor GH_TOKEN is set, build_client should fail.
        // We can only test this reliably if neither is set in the environment.
        if std::env::var("GITHUB_TOKEN").is_err() && std::env::var("GH_TOKEN").is_err() {
            let result = build_client();
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("GITHUB_TOKEN"));
        }
        // If tokens are set, we just verify build_client succeeds
        else {
            let result = build_client();
            assert!(result.is_ok());
        }
    }
}
