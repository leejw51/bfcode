//! Skills system for bfcode CLI
//!
//! Skills are markdown files with YAML-style frontmatter stored in
//! `~/.bfcode/skills/` (global) and `.bfcode/skills/` (project-local).
//! Each skill defines a reusable prompt template that can be triggered
//! by name or keyword pattern, following Anthropic's SKILL.md convention.

use anyhow::{Context, Result};
use colored::Colorize;
use std::fs;
use std::path::{Path, PathBuf};

/// A parsed skill loaded from a markdown file.
#[derive(Debug, Clone)]
pub struct Skill {
    /// Display name of the skill (from frontmatter `name:` field).
    pub name: String,
    /// Short description of what the skill does (from frontmatter `description:` field).
    pub description: String,
    /// Optional regex or keyword pattern that triggers this skill (from frontmatter `trigger:` field).
    pub trigger: Option<String>,
    /// The prompt template body (everything after the frontmatter).
    pub content: String,
    /// Absolute path to the source `.md` file.
    pub path: PathBuf,
}

/// Get the global skills directory (`~/.bfcode/skills/`).
///
/// Creates the directory if it does not exist.
pub fn skills_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    let dir = home.join(".bfcode").join("skills");
    if !dir.exists() {
        fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create skills directory: {}", dir.display()))?;
    }
    Ok(dir)
}

/// Load all skills from both `~/.bfcode/skills/` (global) and `.bfcode/skills/` (project-local).
///
/// Silently skips files that fail to parse. Project-local skills are loaded after
/// global skills so they can shadow globals when matched by name.
pub fn load_skills() -> Vec<Skill> {
    let mut skills = Vec::new();

    // Global skills
    if let Ok(global_dir) = skills_dir() {
        load_skills_from_dir(&global_dir, &mut skills);
    }

    // Project-local skills
    let local_dir = PathBuf::from(".bfcode/skills");
    if local_dir.is_dir() {
        load_skills_from_dir(&local_dir, &mut skills);
    }

    skills
}

/// Load `.md` skill files from a single directory into the provided vec.
fn load_skills_from_dir(dir: &Path, skills: &mut Vec<Skill>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("md") {
            if let Some(skill) = parse_skill_file(&path) {
                skills.push(skill);
            }
        }
    }
}

/// Find a skill by name using case-insensitive partial matching.
///
/// Returns the first skill whose name contains `query` (case-insensitive).
/// An exact case-insensitive match is preferred over a partial match.
pub fn find_skill<'a>(skills: &'a [Skill], query: &str) -> Option<&'a Skill> {
    let query_lower = query.to_lowercase();

    // Prefer exact match first
    if let Some(skill) = skills.iter().find(|s| s.name.to_lowercase() == query_lower) {
        return Some(skill);
    }

    // Fall back to partial match
    skills
        .iter()
        .find(|s| s.name.to_lowercase().contains(&query_lower))
}

/// Format the skills list for terminal display with colors.
pub fn format_skills_list(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return "No skills installed.\n\
                \n\
                Add skills by placing .md files in ~/.bfcode/skills/ or .bfcode/skills/\n\
                Or import with: bfcode skill import <path>"
            .to_string();
    }

    let mut out = String::new();
    out.push_str(&format!("{}\n\n", "Installed Skills".bold().underline()));

    for (i, skill) in skills.iter().enumerate() {
        let idx = format!("  {}.", i + 1);
        let name = skill.name.bold().cyan();
        out.push_str(&format!("{} {}\n", idx, name));
        out.push_str(&format!("     {}\n", skill.description.dimmed()));

        if let Some(trigger) = &skill.trigger {
            out.push_str(&format!("     trigger: {}\n", trigger.yellow()));
        }

        let source = if skill.path.to_string_lossy().contains("/.bfcode/skills/") {
            "global".dimmed()
        } else {
            "project".green()
        };
        out.push_str(&format!(
            "     [{}] {}\n",
            source,
            skill.path.display().to_string().dimmed()
        ));
        out.push('\n');
    }

    out.push_str(&format!(
        "{} skill(s) loaded",
        skills.len().to_string().bold()
    ));

    out
}

/// Import skills from a folder or zip file (auto-detected).
///
/// Returns the list of imported skill file names.
pub fn import_skills(source: &Path) -> Result<Vec<String>> {
    if source
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("zip"))
        .unwrap_or(false)
    {
        import_from_zip(source)
    } else if source.is_dir() {
        import_from_folder(source)
    } else {
        anyhow::bail!(
            "Source must be a directory or .zip file: {}",
            source.display()
        );
    }
}

/// Import skills by copying `.md` files from a folder into `~/.bfcode/skills/`.
///
/// Returns the list of imported file names.
pub fn import_from_folder(source: &Path) -> Result<Vec<String>> {
    anyhow::ensure!(
        source.is_dir(),
        "Source is not a directory: {}",
        source.display()
    );

    let dest = skills_dir()?;
    let mut imported = Vec::new();

    for entry in fs::read_dir(source)
        .with_context(|| format!("Failed to read directory: {}", source.display()))?
    {
        let entry = entry?;
        let path = entry.path();

        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        // Validate that the file has valid frontmatter before importing
        if parse_skill_file(&path).is_none() {
            eprintln!(
                "Skipping {} (invalid or missing frontmatter)",
                path.display()
            );
            continue;
        }

        let file_name = match path.file_name() {
            Some(name) => name.to_owned(),
            None => continue,
        };

        let dest_path = dest.join(&file_name);
        fs::copy(&path, &dest_path).with_context(|| {
            format!(
                "Failed to copy {} -> {}",
                path.display(),
                dest_path.display()
            )
        })?;

        imported.push(file_name.to_string_lossy().to_string());
    }

    Ok(imported)
}

/// Import skills from a zip archive by extracting `.md` files into `~/.bfcode/skills/`.
///
/// Uses the system `unzip` command to avoid adding a zip crate dependency.
/// Extracts to a temporary directory, then copies valid `.md` skill files.
pub fn import_from_zip(zip_path: &Path) -> Result<Vec<String>> {
    anyhow::ensure!(
        zip_path.is_file(),
        "Zip file not found: {}",
        zip_path.display()
    );

    // Create a temp directory for extraction
    let tmp_dir = std::env::temp_dir().join(format!("bfcode-skill-import-{}", std::process::id()));
    if tmp_dir.exists() {
        fs::remove_dir_all(&tmp_dir)?;
    }
    fs::create_dir_all(&tmp_dir)?;

    // Extract using system unzip
    let output = std::process::Command::new("unzip")
        .arg("-o") // overwrite without prompting
        .arg("-q") // quiet
        .arg(zip_path.as_os_str())
        .arg("-d")
        .arg(tmp_dir.as_os_str())
        .output()
        .context("Failed to run `unzip`. Is it installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        fs::remove_dir_all(&tmp_dir).ok();
        anyhow::bail!("unzip failed: {}", stderr.trim());
    }

    // Collect all .md files from the extracted tree (may be nested)
    let md_files = collect_md_files(&tmp_dir);

    let dest = skills_dir()?;
    let mut imported = Vec::new();

    for md_path in &md_files {
        if parse_skill_file(md_path).is_none() {
            continue;
        }

        let file_name = match md_path.file_name() {
            Some(name) => name.to_owned(),
            None => continue,
        };

        let dest_path = dest.join(&file_name);
        fs::copy(md_path, &dest_path).with_context(|| {
            format!(
                "Failed to copy {} -> {}",
                md_path.display(),
                dest_path.display()
            )
        })?;

        imported.push(file_name.to_string_lossy().to_string());
    }

    // Clean up temp directory
    fs::remove_dir_all(&tmp_dir).ok();

    Ok(imported)
}

/// Recursively collect all `.md` files under a directory.
fn collect_md_files(dir: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                results.extend(collect_md_files(&path));
            } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
                results.push(path);
            }
        }
    }
    results
}

/// Parse a single skill markdown file with YAML-style frontmatter.
///
/// Expected format:
/// ```text
/// ---
/// name: My Skill
/// description: Does something useful
/// trigger: /myskill
/// ---
/// Prompt template body goes here...
/// ```
///
/// Returns `None` if the file cannot be read or lacks required frontmatter fields
/// (`name` and `description`).
fn parse_skill_file(path: &Path) -> Option<Skill> {
    let raw = fs::read_to_string(path).ok()?;
    let trimmed = raw.trim_start();

    // Must start with frontmatter delimiter
    if !trimmed.starts_with("---") {
        return None;
    }

    // Find the closing delimiter
    let after_first = &trimmed[3..].trim_start_matches(['\r', '\n']);
    let end_idx = after_first.find("\n---")?;

    let frontmatter_block = &after_first[..end_idx];
    let content_start = end_idx + 4; // skip past "\n---"
    let content = after_first[content_start..]
        .trim_start_matches(['\r', '\n'])
        .to_string();

    // Parse frontmatter key-value pairs
    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut trigger: Option<String> = None;

    for line in frontmatter_block.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_lowercase();
            let value = value.trim().to_string();

            // Strip optional surrounding quotes
            let value = strip_quotes(&value);

            match key.as_str() {
                "name" => name = Some(value),
                "description" => description = Some(value),
                "trigger" => {
                    if !value.is_empty() {
                        trigger = Some(value);
                    }
                }
                _ => {} // ignore unknown fields
            }
        }
    }

    let name = name.filter(|s| !s.is_empty())?;
    let description = description.filter(|s| !s.is_empty())?;

    Some(Skill {
        name,
        description,
        trigger,
        content,
        path: path.to_path_buf(),
    })
}

/// Strip matching surrounding single or double quotes from a string.
fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 {
        if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_skill_file(dir: &Path, filename: &str, contents: &str) -> PathBuf {
        let path = dir.join(filename);
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        path
    }

    #[test]
    fn test_parse_skill_file_basic() {
        let dir = std::env::temp_dir().join("bfcode-test-skill-parse");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let path = write_skill_file(
            &dir,
            "test.md",
            "---\n\
             name: Test Skill\n\
             description: A test skill for unit testing\n\
             trigger: /test\n\
             ---\n\
             You are a helpful assistant.\n\
             Do the thing.\n",
        );

        let skill = parse_skill_file(&path).expect("should parse");
        assert_eq!(skill.name, "Test Skill");
        assert_eq!(skill.description, "A test skill for unit testing");
        assert_eq!(skill.trigger.as_deref(), Some("/test"));
        assert!(skill.content.contains("You are a helpful assistant."));
        assert!(skill.content.contains("Do the thing."));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_parse_skill_file_no_trigger() {
        let dir = std::env::temp_dir().join("bfcode-test-skill-notrigger");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let path = write_skill_file(
            &dir,
            "notrigger.md",
            "---\n\
             name: Simple\n\
             description: No trigger field\n\
             ---\n\
             Content here.\n",
        );

        let skill = parse_skill_file(&path).expect("should parse");
        assert_eq!(skill.name, "Simple");
        assert!(skill.trigger.is_none());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_parse_skill_file_missing_name() {
        let dir = std::env::temp_dir().join("bfcode-test-skill-noname");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let path = write_skill_file(
            &dir,
            "bad.md",
            "---\n\
             description: Missing name\n\
             ---\n\
             Content.\n",
        );

        assert!(parse_skill_file(&path).is_none());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_parse_skill_file_no_frontmatter() {
        let dir = std::env::temp_dir().join("bfcode-test-skill-nofm");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let path = write_skill_file(&dir, "plain.md", "Just a regular markdown file.\n");

        assert!(parse_skill_file(&path).is_none());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_parse_skill_file_quoted_values() {
        let dir = std::env::temp_dir().join("bfcode-test-skill-quotes");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let path = write_skill_file(
            &dir,
            "quoted.md",
            "---\n\
             name: \"Quoted Skill\"\n\
             description: 'Single quoted description'\n\
             ---\n\
             Body.\n",
        );

        let skill = parse_skill_file(&path).expect("should parse");
        assert_eq!(skill.name, "Quoted Skill");
        assert_eq!(skill.description, "Single quoted description");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_find_skill_exact() {
        let skills = vec![
            Skill {
                name: "commit".into(),
                description: "Generate commits".into(),
                trigger: None,
                content: String::new(),
                path: PathBuf::from("/test/commit.md"),
            },
            Skill {
                name: "review-pr".into(),
                description: "Review pull requests".into(),
                trigger: None,
                content: String::new(),
                path: PathBuf::from("/test/review.md"),
            },
        ];

        let found = find_skill(&skills, "commit");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "commit");
    }

    #[test]
    fn test_find_skill_partial() {
        let skills = vec![Skill {
            name: "review-pr".into(),
            description: "Review pull requests".into(),
            trigger: None,
            content: String::new(),
            path: PathBuf::from("/test/review.md"),
        }];

        let found = find_skill(&skills, "review");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "review-pr");
    }

    #[test]
    fn test_find_skill_case_insensitive() {
        let skills = vec![Skill {
            name: "Commit".into(),
            description: "Generate commits".into(),
            trigger: None,
            content: String::new(),
            path: PathBuf::from("/test/commit.md"),
        }];

        let found = find_skill(&skills, "COMMIT");
        assert!(found.is_some());
    }

    #[test]
    fn test_find_skill_not_found() {
        let skills = vec![Skill {
            name: "commit".into(),
            description: "Generate commits".into(),
            trigger: None,
            content: String::new(),
            path: PathBuf::from("/test/commit.md"),
        }];

        assert!(find_skill(&skills, "deploy").is_none());
    }

    #[test]
    fn test_format_skills_list_empty() {
        let output = format_skills_list(&[]);
        assert!(output.contains("No skills installed"));
    }

    #[test]
    fn test_format_skills_list_with_skills() {
        let skills = vec![Skill {
            name: "commit".into(),
            description: "Generate commits".into(),
            trigger: Some("/commit".into()),
            content: String::new(),
            path: PathBuf::from("/home/user/.bfcode/skills/commit.md"),
        }];

        let output = format_skills_list(&skills);
        assert!(output.contains("commit"));
        assert!(output.contains("Generate commits"));
        assert!(output.contains("/commit"));
        assert!(output.contains("skill(s) loaded"));
    }

    #[test]
    fn test_import_from_folder() {
        let src = std::env::temp_dir().join("bfcode-test-import-src");
        let _ = fs::remove_dir_all(&src);
        fs::create_dir_all(&src).unwrap();

        write_skill_file(
            &src,
            "good.md",
            "---\nname: Good\ndescription: A good skill\n---\nBody.\n",
        );
        write_skill_file(&src, "not-a-skill.txt", "plain text file");
        write_skill_file(&src, "bad.md", "No frontmatter here");

        // This test touches the real ~/.bfcode/skills/ directory,
        // so we just verify it doesn't panic and returns expected count.
        let result = import_from_folder(&src);
        assert!(result.is_ok());
        let imported = result.unwrap();
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0], "good.md");

        fs::remove_dir_all(&src).ok();
    }

    #[test]
    fn test_strip_quotes() {
        assert_eq!(strip_quotes("\"hello\""), "hello");
        assert_eq!(strip_quotes("'world'"), "world");
        assert_eq!(strip_quotes("no quotes"), "no quotes");
        assert_eq!(strip_quotes("\"mismatched'"), "\"mismatched'");
        assert_eq!(strip_quotes(""), "");
    }
}
