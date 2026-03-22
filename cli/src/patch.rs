//! Patch parsing and application module.
//!
//! Supports two patch formats:
//! 1. Standard unified diff (`--- a/file`, `+++ b/file`, `@@ hunks`)
//! 2. OpenCode-style structured patch (`*** Begin Patch` / `*** End Patch`)
//!    with Add/Delete/Update/Move file operations and multi-pass fuzzy matching.

use anyhow::{Context, Result, bail};

// ─── Types ───────────────────────────────────────────────────────────

/// Result of parsing a patch: a list of file-level operations.
#[derive(Debug)]
pub enum FileOp {
    /// Create a new file with the given content.
    Add { path: String, contents: String },
    /// Delete an existing file.
    Delete { path: String },
    /// Modify (and optionally move/rename) an existing file.
    Update {
        path: String,
        move_path: Option<String>,
        chunks: Vec<UpdateChunk>,
    },
    /// Legacy unified-diff hunk (from standard format).
    UnifiedHunk {
        path: String,
        hunks: Vec<UnifiedHunk>,
    },
}

/// A chunk inside an Update operation (OpenCode-style).
#[derive(Debug, Clone)]
pub struct UpdateChunk {
    pub old_lines: Vec<String>,
    pub new_lines: Vec<String>,
    pub change_context: Option<String>,
    pub is_end_of_file: bool,
}

/// A hunk from standard unified diff format.
#[derive(Debug)]
pub struct UnifiedHunk {
    pub old_start: usize,
    pub _old_count: usize,
    pub _new_start: usize,
    pub _new_count: usize,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone)]
pub enum DiffLine {
    Context(String),
    Add(String),
    Remove(String),
}

// ─── Top-level parse ─────────────────────────────────────────────────

/// Parse a patch string, auto-detecting the format.
/// Returns a list of file operations to apply.
pub fn parse_patch(patch: &str) -> Result<Vec<FileOp>> {
    let trimmed = patch.trim();
    if trimmed.is_empty() {
        bail!("No valid patches found in input");
    }

    // Strip heredoc wrapper if present
    let cleaned = strip_heredoc(trimmed);

    // Detect format
    if cleaned.contains("*** Begin Patch") {
        parse_opencode_patch(&cleaned)
    } else if cleaned.contains("--- ") && cleaned.contains("+++ ") {
        parse_unified_diff(&cleaned)
    } else if cleaned.contains("*** Begin Patch") {
        // Already handled above, but just in case
        parse_opencode_patch(&cleaned)
    } else {
        bail!("Unrecognized patch format: expected unified diff or *** Begin Patch markers");
    }
}

/// Apply a list of file operations to the filesystem.
pub async fn apply_file_ops(ops: &[FileOp], session_id: &str) -> Result<Vec<String>> {
    let mut results = Vec::new();

    for op in ops {
        match op {
            FileOp::Add { path, contents } => {
                crate::tools::save_file_snapshot(path, session_id);
                if let Some(parent) = std::path::Path::new(path).parent() {
                    if !parent.as_os_str().is_empty() {
                        tokio::fs::create_dir_all(parent).await?;
                    }
                }
                tokio::fs::write(path, contents)
                    .await
                    .with_context(|| format!("writing {path}"))?;
                results.push(format!("A {path}"));
            }
            FileOp::Delete { path } => {
                crate::tools::save_file_snapshot(path, session_id);
                tokio::fs::remove_file(path)
                    .await
                    .with_context(|| format!("deleting {path}"))?;
                results.push(format!("D {path}"));
            }
            FileOp::Update {
                path,
                move_path,
                chunks,
            } => {
                crate::tools::save_file_snapshot(path, session_id);
                let content = if std::path::Path::new(path).exists() {
                    tokio::fs::read_to_string(path)
                        .await
                        .with_context(|| format!("reading {path}"))?
                } else {
                    String::new()
                };

                let new_content = apply_update_chunks(&content, path, chunks)?;

                let target = move_path.as_deref().unwrap_or(path);
                if let Some(parent) = std::path::Path::new(target).parent() {
                    if !parent.as_os_str().is_empty() {
                        tokio::fs::create_dir_all(parent).await?;
                    }
                }

                tokio::fs::write(target, &new_content)
                    .await
                    .with_context(|| format!("writing {target}"))?;

                if move_path.is_some() && target != path {
                    tokio::fs::remove_file(path)
                        .await
                        .with_context(|| format!("removing old path {path}"))?;
                    results.push(format!("R {path} -> {target} ({} chunks)", chunks.len()));
                } else {
                    results.push(format!("M {target} ({} chunks)", chunks.len()));
                }
            }
            FileOp::UnifiedHunk { path, hunks } => {
                crate::tools::save_file_snapshot(path, session_id);
                let content = if std::path::Path::new(path).exists() {
                    tokio::fs::read_to_string(path).await.unwrap_or_default()
                } else {
                    String::new()
                };

                let patched = apply_unified_hunks(&content, hunks)?;

                if let Some(parent) = std::path::Path::new(path).parent() {
                    if !parent.as_os_str().is_empty() {
                        tokio::fs::create_dir_all(parent).await?;
                    }
                }

                tokio::fs::write(path, &patched)
                    .await
                    .with_context(|| format!("writing {path}"))?;

                let status = if content.is_empty() { "A" } else { "M" };
                results.push(format!("{status} {path} ({} hunks applied)", hunks.len()));
            }
        }
    }

    Ok(results)
}

/// Count the number of files affected by a patch string (for permission summaries).
pub fn count_affected_files(patch: &str) -> usize {
    let trimmed = patch.trim();
    let cleaned = strip_heredoc(trimmed);
    if cleaned.contains("*** Begin Patch") {
        // Count *** Add/Delete/Update File: lines
        cleaned
            .lines()
            .filter(|l| {
                l.starts_with("*** Add File:")
                    || l.starts_with("*** Delete File:")
                    || l.starts_with("*** Update File:")
            })
            .count()
    } else {
        // Count +++ lines (unified diff)
        cleaned.matches("+++ ").count()
    }
}

// ─── Heredoc stripping ───────────────────────────────────────────────

fn strip_heredoc(input: &str) -> String {
    // Match: cat <<'EOF'\n...\nEOF  or  <<EOF\n...\nEOF
    let lines: Vec<&str> = input.lines().collect();
    if lines.len() < 3 {
        return input.to_string();
    }

    // Check first line for heredoc pattern
    let first = lines[0].trim();
    let heredoc_tag = if let Some(rest) = first.strip_prefix("cat ") {
        extract_heredoc_tag(rest.trim())
    } else {
        extract_heredoc_tag(first)
    };

    if let Some(tag) = heredoc_tag {
        // Find matching end tag
        if let Some(end_idx) = lines.iter().rposition(|l| l.trim() == tag) {
            if end_idx > 0 {
                return lines[1..end_idx].join("\n");
            }
        }
    }

    input.to_string()
}

fn extract_heredoc_tag(s: &str) -> Option<&str> {
    let rest = s.strip_prefix("<<")?;
    let rest = rest.trim();
    // Remove quotes around the tag
    if rest.starts_with('\'') && rest.ends_with('\'') && rest.len() > 2 {
        Some(&rest[1..rest.len() - 1])
    } else if rest.starts_with('"') && rest.ends_with('"') && rest.len() > 2 {
        Some(&rest[1..rest.len() - 1])
    } else if rest.chars().all(|c| c.is_alphanumeric() || c == '_') && !rest.is_empty() {
        Some(rest)
    } else {
        None
    }
}

// ─── OpenCode-style patch parser ─────────────────────────────────────

fn parse_opencode_patch(input: &str) -> Result<Vec<FileOp>> {
    let lines: Vec<&str> = input.lines().collect();
    let begin_idx = lines
        .iter()
        .position(|l| l.trim() == "*** Begin Patch")
        .ok_or_else(|| anyhow::anyhow!("Missing *** Begin Patch marker"))?;
    let end_idx = lines
        .iter()
        .position(|l| l.trim() == "*** End Patch")
        .ok_or_else(|| anyhow::anyhow!("Missing *** End Patch marker"))?;

    if begin_idx >= end_idx {
        bail!("*** Begin Patch must come before *** End Patch");
    }

    let mut ops = Vec::new();
    let mut i = begin_idx + 1;

    while i < end_idx {
        let line = lines[i];

        if let Some(path) = line.strip_prefix("*** Add File:") {
            let path = path.trim().to_string();
            i += 1;
            let (contents, next_i) = parse_add_content(&lines, i, end_idx);
            ops.push(FileOp::Add { path, contents });
            i = next_i;
        } else if let Some(path) = line.strip_prefix("*** Delete File:") {
            let path = path.trim().to_string();
            ops.push(FileOp::Delete { path });
            i += 1;
        } else if let Some(path) = line.strip_prefix("*** Update File:") {
            let path = path.trim().to_string();
            i += 1;

            // Check for move directive
            let move_path = if i < end_idx {
                if let Some(mp) = lines[i].strip_prefix("*** Move to:") {
                    i += 1;
                    Some(mp.trim().to_string())
                } else {
                    None
                }
            } else {
                None
            };

            let (chunks, next_i) = parse_update_chunks(&lines, i, end_idx);
            ops.push(FileOp::Update {
                path,
                move_path,
                chunks,
            });
            i = next_i;
        } else {
            i += 1;
        }
    }

    if ops.is_empty() {
        bail!("No file operations found between Begin/End Patch markers");
    }

    Ok(ops)
}

/// Check if a line is a file-operation header (*** Add/Delete/Update/End Patch).
/// This does NOT match *** End of File, which is an in-chunk marker.
fn is_file_op_header(line: &str) -> bool {
    line.starts_with("*** Add File:")
        || line.starts_with("*** Delete File:")
        || line.starts_with("*** Update File:")
        || line.starts_with("*** End Patch")
        || line.starts_with("*** Move to:")
}

fn parse_add_content(lines: &[&str], start: usize, end: usize) -> (String, usize) {
    let mut content_lines = Vec::new();
    let mut i = start;

    while i < end && !is_file_op_header(lines[i]) {
        if let Some(rest) = lines[i].strip_prefix('+') {
            content_lines.push(rest.to_string());
        }
        i += 1;
    }

    (content_lines.join("\n"), i)
}

fn parse_update_chunks(lines: &[&str], start: usize, end: usize) -> (Vec<UpdateChunk>, usize) {
    let mut chunks = Vec::new();
    let mut i = start;

    while i < end && !is_file_op_header(lines[i]) {
        if lines[i].starts_with("@@") {
            // Extract context from @@ line
            let context_text = lines[i][2..].trim().to_string();
            let change_context = if context_text.is_empty() {
                None
            } else {
                Some(context_text)
            };
            i += 1;

            let mut old_lines = Vec::new();
            let mut new_lines = Vec::new();
            let mut is_end_of_file = false;

            while i < end && !lines[i].starts_with("@@") && !is_file_op_header(lines[i]) {
                let line = lines[i];

                if line == "*** End of File" {
                    is_end_of_file = true;
                    i += 1;
                    break;
                }

                if let Some(rest) = line.strip_prefix(' ') {
                    // Context / keep line
                    old_lines.push(rest.to_string());
                    new_lines.push(rest.to_string());
                } else if let Some(rest) = line.strip_prefix('-') {
                    old_lines.push(rest.to_string());
                } else if let Some(rest) = line.strip_prefix('+') {
                    new_lines.push(rest.to_string());
                }

                i += 1;
            }

            chunks.push(UpdateChunk {
                old_lines,
                new_lines,
                change_context,
                is_end_of_file,
            });
        } else {
            i += 1;
        }
    }

    (chunks, i)
}

// ─── Apply OpenCode-style update chunks ──────────────────────────────

fn apply_update_chunks(content: &str, file_path: &str, chunks: &[UpdateChunk]) -> Result<String> {
    let mut original_lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();

    // Drop trailing empty element for consistent counting
    if original_lines.last().map(|l| l.as_str()) == Some("") && content.ends_with('\n') {
        original_lines.pop();
    }

    let replacements = compute_replacements(&original_lines, file_path, chunks)?;
    let mut new_lines = apply_replacements(&original_lines, &replacements);

    // Ensure trailing newline
    if new_lines.is_empty() || new_lines.last().map(|l| l.as_str()) != Some("") {
        new_lines.push(String::new());
    }

    Ok(new_lines.join("\n"))
}

fn compute_replacements(
    original_lines: &[String],
    file_path: &str,
    chunks: &[UpdateChunk],
) -> Result<Vec<(usize, usize, Vec<String>)>> {
    let mut replacements = Vec::new();
    let mut line_index = 0;

    for chunk in chunks {
        // Handle context-based seeking
        if let Some(ctx) = &chunk.change_context {
            let ctx_idx = seek_sequence(original_lines, &[ctx.clone()], line_index, false);
            if ctx_idx < 0 {
                bail!("Failed to find context '{ctx}' in {file_path}");
            }
            line_index = ctx_idx as usize + 1;
        }

        // Pure addition (no old lines)
        if chunk.old_lines.is_empty() {
            let insertion_idx = if original_lines.last().map(|l| l.as_str()) == Some("") {
                original_lines.len() - 1
            } else {
                original_lines.len()
            };
            replacements.push((insertion_idx, 0, chunk.new_lines.clone()));
            continue;
        }

        let mut pattern = chunk.old_lines.clone();
        let mut new_slice = chunk.new_lines.clone();
        let mut found = seek_sequence(original_lines, &pattern, line_index, chunk.is_end_of_file);

        // Retry without trailing empty line
        if found < 0 && !pattern.is_empty() && pattern.last().map(|l| l.as_str()) == Some("") {
            pattern.pop();
            if !new_slice.is_empty() && new_slice.last().map(|l| l.as_str()) == Some("") {
                new_slice.pop();
            }
            found = seek_sequence(original_lines, &pattern, line_index, chunk.is_end_of_file);
        }

        if found >= 0 {
            let found_idx = found as usize;
            replacements.push((found_idx, pattern.len(), new_slice));
            line_index = found_idx + pattern.len();
        } else {
            let preview: String = chunk
                .old_lines
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            bail!("Failed to find expected lines in {file_path}:\n{preview}");
        }
    }

    replacements.sort_by_key(|r| r.0);
    Ok(replacements)
}

fn apply_replacements(
    lines: &[String],
    replacements: &[(usize, usize, Vec<String>)],
) -> Vec<String> {
    let mut result: Vec<String> = lines.to_vec();

    // Apply in reverse order to preserve indices
    for (start_idx, old_len, new_segment) in replacements.iter().rev() {
        let end = (*start_idx + *old_len).min(result.len());
        result.splice(*start_idx..end, new_segment.iter().cloned());
    }

    result
}

// ─── Multi-pass fuzzy sequence matching ──────────────────────────────

/// Normalize Unicode punctuation to ASCII equivalents.
fn normalize_unicode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}' => '-',
            '\u{00A0}' => ' ',
            _ => c,
        })
        .collect::<String>()
        .replace('\u{2026}', "...")
}

fn try_match<F>(
    lines: &[String],
    pattern: &[String],
    start_index: usize,
    compare: F,
    eof: bool,
) -> isize
where
    F: Fn(&str, &str) -> bool,
{
    if pattern.is_empty() {
        return -1;
    }

    // If EOF anchor, try matching from end first
    if eof && lines.len() >= pattern.len() {
        let from_end = lines.len() - pattern.len();
        if from_end >= start_index {
            let matches = pattern
                .iter()
                .enumerate()
                .all(|(j, p)| compare(&lines[from_end + j], p));
            if matches {
                return from_end as isize;
            }
        }
    }

    // Forward search
    if lines.len() < pattern.len() {
        return -1;
    }
    let limit = lines.len() - pattern.len();
    for i in start_index..=limit {
        let matches = pattern
            .iter()
            .enumerate()
            .all(|(j, p)| compare(&lines[i + j], p));
        if matches {
            return i as isize;
        }
    }

    -1
}

/// Multi-pass sequence search with progressively looser matching:
/// 1. Exact match
/// 2. Trailing-whitespace-trimmed
/// 3. Fully trimmed
/// 4. Unicode-normalized + trimmed
fn seek_sequence(lines: &[String], pattern: &[String], start_index: usize, eof: bool) -> isize {
    if pattern.is_empty() {
        return -1;
    }

    // Pass 1: exact
    let exact = try_match(lines, pattern, start_index, |a, b| a == b, eof);
    if exact >= 0 {
        return exact;
    }

    // Pass 2: rstrip
    let rstrip = try_match(
        lines,
        pattern,
        start_index,
        |a, b| a.trim_end() == b.trim_end(),
        eof,
    );
    if rstrip >= 0 {
        return rstrip;
    }

    // Pass 3: trim both ends
    let trim = try_match(
        lines,
        pattern,
        start_index,
        |a, b| a.trim() == b.trim(),
        eof,
    );
    if trim >= 0 {
        return trim;
    }

    // Pass 4: unicode normalized + trimmed
    try_match(
        lines,
        pattern,
        start_index,
        |a, b| normalize_unicode(a.trim()) == normalize_unicode(b.trim()),
        eof,
    )
}

// ─── Standard unified diff parser ────────────────────────────────────

fn parse_unified_diff(patch: &str) -> Result<Vec<FileOp>> {
    let mut file_ops = Vec::new();
    let lines: Vec<&str> = patch.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        // Look for --- header
        if lines[i].starts_with("--- ") && i + 1 < lines.len() && lines[i + 1].starts_with("+++ ") {
            let _old_path = lines[i].strip_prefix("--- ").unwrap_or("").trim();
            let new_path = lines[i + 1].strip_prefix("+++ ").unwrap_or("").trim();

            // Strip a/ b/ prefixes
            let target = new_path
                .strip_prefix("b/")
                .or_else(|| new_path.strip_prefix("a/"))
                .unwrap_or(new_path)
                .to_string();

            i += 2;

            let mut hunks = Vec::new();

            // Parse hunks
            while i < lines.len() && !lines[i].starts_with("--- ") {
                if lines[i].starts_with("@@ ") {
                    let hunk_header = lines[i];
                    let (old_start, old_count, new_start, new_count) =
                        parse_hunk_header(hunk_header)?;
                    i += 1;

                    let mut hunk_lines = Vec::new();
                    while i < lines.len()
                        && !lines[i].starts_with("@@ ")
                        && !lines[i].starts_with("--- ")
                    {
                        let line = lines[i];
                        if line.starts_with('+') {
                            hunk_lines.push(DiffLine::Add(line[1..].to_string()));
                        } else if line.starts_with('-') {
                            hunk_lines.push(DiffLine::Remove(line[1..].to_string()));
                        } else if line.starts_with(' ') {
                            hunk_lines.push(DiffLine::Context(line[1..].to_string()));
                        } else if line == "\\ No newline at end of file" {
                            // Skip
                        } else if line.is_empty() {
                            hunk_lines.push(DiffLine::Context(String::new()));
                        } else {
                            break;
                        }
                        i += 1;
                    }

                    hunks.push(UnifiedHunk {
                        old_start,
                        _old_count: old_count,
                        _new_start: new_start,
                        _new_count: new_count,
                        lines: hunk_lines,
                    });
                } else {
                    i += 1;
                }
            }

            if !hunks.is_empty() || target != "/dev/null" {
                file_ops.push(FileOp::UnifiedHunk {
                    path: target,
                    hunks,
                });
            }
        } else {
            i += 1;
        }
    }

    if file_ops.is_empty() {
        bail!("No valid patches found in input");
    }

    Ok(file_ops)
}

pub fn parse_hunk_header(header: &str) -> Result<(usize, usize, usize, usize)> {
    let parts: Vec<&str> = header.split("@@").collect();
    if parts.len() < 2 {
        bail!("Invalid hunk header: {header}");
    }
    let range_part = parts[1].trim();
    let ranges: Vec<&str> = range_part.split_whitespace().collect();
    if ranges.len() < 2 {
        bail!("Invalid hunk ranges: {range_part}");
    }

    let old = parse_range(ranges[0].strip_prefix('-').unwrap_or(ranges[0]))?;
    let new = parse_range(ranges[1].strip_prefix('+').unwrap_or(ranges[1]))?;

    Ok((old.0, old.1, new.0, new.1))
}

pub fn parse_range(s: &str) -> Result<(usize, usize)> {
    if let Some((start, count)) = s.split_once(',') {
        Ok((start.parse()?, count.parse()?))
    } else {
        let start: usize = s.parse()?;
        Ok((start, 1))
    }
}

/// Apply unified diff hunks to content string.
pub fn apply_unified_hunks(content: &str, hunks: &[UnifiedHunk]) -> Result<String> {
    let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();

    // Apply hunks in reverse order to preserve line numbers
    let mut sorted_hunks: Vec<&UnifiedHunk> = hunks.iter().collect();
    sorted_hunks.sort_by(|a, b| b.old_start.cmp(&a.old_start));

    for hunk in sorted_hunks {
        let start_idx = if hunk.old_start == 0 {
            0
        } else {
            hunk.old_start - 1
        };

        // Rebuild the affected region
        let mut new_lines = Vec::new();

        for diff_line in &hunk.lines {
            match diff_line {
                DiffLine::Context(text) => {
                    new_lines.push(text.clone());
                }
                DiffLine::Remove(_) => {
                    // Skip removed lines
                }
                DiffLine::Add(text) => {
                    new_lines.push(text.clone());
                }
            }
        }

        // Count context + remove lines to know how many old lines to replace
        let old_line_count = hunk
            .lines
            .iter()
            .filter(|l| !matches!(l, DiffLine::Add(_)))
            .count();
        let end_idx = (start_idx + old_line_count).min(lines.len());

        lines.splice(start_idx..end_idx, new_lines);
    }

    let mut result = lines.join("\n");
    if content.ends_with('\n') && !result.ends_with('\n') {
        result.push('\n');
    }
    Ok(result)
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Format detection ─────────────────────────────────────────────

    #[test]
    fn test_parse_empty_patch_errors() {
        assert!(parse_patch("").is_err());
        assert!(parse_patch("   ").is_err());
    }

    #[test]
    fn test_parse_unrecognized_format_errors() {
        assert!(parse_patch("just some random text").is_err());
    }

    #[test]
    fn test_parse_detects_unified_diff() {
        let patch = "--- a/foo.txt\n+++ b/foo.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n";
        let ops = parse_patch(patch).unwrap();
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], FileOp::UnifiedHunk { path, .. } if path == "foo.txt"));
    }

    #[test]
    fn test_parse_detects_opencode_format() {
        let patch = "\
*** Begin Patch
*** Add File: hello.txt
+hello world
*** End Patch";
        let ops = parse_patch(patch).unwrap();
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], FileOp::Add { path, .. } if path == "hello.txt"));
    }

    // ── Heredoc stripping ────────────────────────────────────────────

    #[test]
    fn test_strip_heredoc_cat_single_quotes() {
        let input = "cat <<'EOF'\nhello\nworld\nEOF";
        assert_eq!(strip_heredoc(input), "hello\nworld");
    }

    #[test]
    fn test_strip_heredoc_double_quotes() {
        let input = "<<\"PATCH\"\nline1\nline2\nPATCH";
        assert_eq!(strip_heredoc(input), "line1\nline2");
    }

    #[test]
    fn test_strip_heredoc_plain() {
        let input = "<<EOF\ncontent here\nEOF";
        assert_eq!(strip_heredoc(input), "content here");
    }

    #[test]
    fn test_strip_heredoc_no_match() {
        let input = "just normal text\nno heredoc here";
        assert_eq!(strip_heredoc(input), input);
    }

    #[test]
    fn test_strip_heredoc_preserves_inner_content() {
        let patch =
            "cat <<'EOF'\n*** Begin Patch\n*** Add File: test.txt\n+content\n*** End Patch\nEOF";
        let stripped = strip_heredoc(patch);
        assert!(stripped.contains("*** Begin Patch"));
        assert!(!stripped.contains("EOF"));
        assert!(!stripped.contains("cat"));
    }

    // ── OpenCode format: Add File ────────────────────────────────────

    #[test]
    fn test_opencode_add_file() {
        let patch = "\
*** Begin Patch
*** Add File: src/new.rs
+fn main() {
+    println!(\"hello\");
+}
*** End Patch";
        let ops = parse_patch(patch).unwrap();
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            FileOp::Add { path, contents } => {
                assert_eq!(path, "src/new.rs");
                assert!(contents.contains("fn main()"));
                assert!(contents.contains("println!"));
            }
            _ => panic!("Expected Add"),
        }
    }

    #[test]
    fn test_opencode_add_multiple_files() {
        let patch = "\
*** Begin Patch
*** Add File: a.txt
+aaa
*** Add File: b.txt
+bbb
*** End Patch";
        let ops = parse_patch(patch).unwrap();
        assert_eq!(ops.len(), 2);
        assert!(
            matches!(&ops[0], FileOp::Add { path, contents } if path == "a.txt" && contents == "aaa")
        );
        assert!(
            matches!(&ops[1], FileOp::Add { path, contents } if path == "b.txt" && contents == "bbb")
        );
    }

    // ── OpenCode format: Delete File ─────────────────────────────────

    #[test]
    fn test_opencode_delete_file() {
        let patch = "\
*** Begin Patch
*** Delete File: old/unused.rs
*** End Patch";
        let ops = parse_patch(patch).unwrap();
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], FileOp::Delete { path } if path == "old/unused.rs"));
    }

    // ── OpenCode format: Update File ─────────────────────────────────

    #[test]
    fn test_opencode_update_basic() {
        let patch = "\
*** Begin Patch
*** Update File: src/lib.rs
@@
 fn hello() {
-    println!(\"old\");
+    println!(\"new\");
 }
*** End Patch";
        let ops = parse_patch(patch).unwrap();
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            FileOp::Update {
                path,
                move_path,
                chunks,
            } => {
                assert_eq!(path, "src/lib.rs");
                assert!(move_path.is_none());
                assert_eq!(chunks.len(), 1);
                assert_eq!(
                    chunks[0].old_lines,
                    vec!["fn hello() {", "    println!(\"old\");", "}"]
                );
                assert_eq!(
                    chunks[0].new_lines,
                    vec!["fn hello() {", "    println!(\"new\");", "}"]
                );
            }
            _ => panic!("Expected Update"),
        }
    }

    #[test]
    fn test_opencode_update_with_move() {
        let patch = "\
*** Begin Patch
*** Update File: old_name.rs
*** Move to: new_name.rs
@@
-old content
+new content
*** End Patch";
        let ops = parse_patch(patch).unwrap();
        match &ops[0] {
            FileOp::Update {
                path, move_path, ..
            } => {
                assert_eq!(path, "old_name.rs");
                assert_eq!(move_path.as_deref(), Some("new_name.rs"));
            }
            _ => panic!("Expected Update"),
        }
    }

    #[test]
    fn test_opencode_update_with_context() {
        let patch = "\
*** Begin Patch
*** Update File: main.rs
@@ fn process
 fn process() {
-    old_call();
+    new_call();
 }
*** End Patch";
        let ops = parse_patch(patch).unwrap();
        match &ops[0] {
            FileOp::Update { chunks, .. } => {
                assert_eq!(chunks[0].change_context.as_deref(), Some("fn process"));
            }
            _ => panic!("Expected Update"),
        }
    }

    #[test]
    fn test_opencode_update_eof_marker() {
        let patch = "\
*** Begin Patch
*** Update File: config.toml
@@
 [dependencies]
-old_dep = \"1.0\"
+new_dep = \"2.0\"
*** End of File
*** End Patch";
        let ops = parse_patch(patch).unwrap();
        match &ops[0] {
            FileOp::Update { chunks, .. } => {
                assert!(chunks[0].is_end_of_file);
            }
            _ => panic!("Expected Update"),
        }
    }

    #[test]
    fn test_opencode_mixed_operations() {
        let patch = "\
*** Begin Patch
*** Add File: new.txt
+new content
*** Delete File: old.txt
*** Update File: existing.txt
@@
-old
+new
*** End Patch";
        let ops = parse_patch(patch).unwrap();
        assert_eq!(ops.len(), 3);
        assert!(matches!(&ops[0], FileOp::Add { .. }));
        assert!(matches!(&ops[1], FileOp::Delete { .. }));
        assert!(matches!(&ops[2], FileOp::Update { .. }));
    }

    #[test]
    fn test_opencode_empty_between_markers_errors() {
        let patch = "*** Begin Patch\n*** End Patch";
        assert!(parse_patch(patch).is_err());
    }

    // ── Multi-pass fuzzy matching ────────────────────────────────────

    #[test]
    fn test_seek_exact_match() {
        let lines: Vec<String> = vec!["aaa", "bbb", "ccc"]
            .into_iter()
            .map(String::from)
            .collect();
        let pattern: Vec<String> = vec!["bbb"].into_iter().map(String::from).collect();
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), 1);
    }

    #[test]
    fn test_seek_trailing_whitespace_match() {
        let lines: Vec<String> = vec!["aaa", "bbb   ", "ccc"]
            .into_iter()
            .map(String::from)
            .collect();
        let pattern: Vec<String> = vec!["bbb"].into_iter().map(String::from).collect();
        // Exact fails, rstrip succeeds
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), 1);
    }

    #[test]
    fn test_seek_trim_match() {
        let lines: Vec<String> = vec!["aaa", "    bbb    ", "ccc"]
            .into_iter()
            .map(String::from)
            .collect();
        let pattern: Vec<String> = vec!["bbb"].into_iter().map(String::from).collect();
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), 1);
    }

    #[test]
    fn test_seek_unicode_normalized_match() {
        let lines: Vec<String> = vec!["aaa", "it\u{2019}s a \u{201C}test\u{201D}", "ccc"]
            .into_iter()
            .map(String::from)
            .collect();
        let pattern: Vec<String> = vec!["it's a \"test\""]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), 1);
    }

    #[test]
    fn test_seek_no_match() {
        let lines: Vec<String> = vec!["aaa", "bbb", "ccc"]
            .into_iter()
            .map(String::from)
            .collect();
        let pattern: Vec<String> = vec!["zzz"].into_iter().map(String::from).collect();
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), -1);
    }

    #[test]
    fn test_seek_eof_anchor() {
        let lines: Vec<String> = vec!["dup", "middle", "dup"]
            .into_iter()
            .map(String::from)
            .collect();
        let pattern: Vec<String> = vec!["dup"].into_iter().map(String::from).collect();
        // Without EOF, finds first (index 0)
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), 0);
        // With EOF, finds last (index 2)
        assert_eq!(seek_sequence(&lines, &pattern, 0, true), 2);
    }

    #[test]
    fn test_seek_multi_line_pattern() {
        let lines: Vec<String> = vec!["aaa", "bbb", "ccc", "ddd"]
            .into_iter()
            .map(String::from)
            .collect();
        let pattern: Vec<String> = vec!["bbb", "ccc"].into_iter().map(String::from).collect();
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), 1);
    }

    #[test]
    fn test_seek_start_index() {
        let lines: Vec<String> = vec!["aaa", "bbb", "aaa", "bbb"]
            .into_iter()
            .map(String::from)
            .collect();
        let pattern: Vec<String> = vec!["aaa"].into_iter().map(String::from).collect();
        // Starting from 0 finds index 0
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), 0);
        // Starting from 1 finds index 2
        assert_eq!(seek_sequence(&lines, &pattern, 1, false), 2);
    }

    #[test]
    fn test_seek_empty_pattern() {
        let lines: Vec<String> = vec!["aaa"].into_iter().map(String::from).collect();
        assert_eq!(seek_sequence(&lines, &[], 0, false), -1);
    }

    // ── Unicode normalization ────────────────────────────────────────

    #[test]
    fn test_normalize_unicode_quotes() {
        assert_eq!(normalize_unicode("\u{201C}hello\u{201D}"), "\"hello\"");
        assert_eq!(normalize_unicode("it\u{2019}s"), "it's");
    }

    #[test]
    fn test_normalize_unicode_dashes() {
        assert_eq!(normalize_unicode("a\u{2014}b"), "a-b"); // em dash
        assert_eq!(normalize_unicode("a\u{2013}b"), "a-b"); // en dash
    }

    #[test]
    fn test_normalize_unicode_ellipsis() {
        assert_eq!(normalize_unicode("wait\u{2026}"), "wait...");
    }

    #[test]
    fn test_normalize_unicode_nbsp() {
        assert_eq!(normalize_unicode("hello\u{00A0}world"), "hello world");
    }

    // ── apply_update_chunks ──────────────────────────────────────────

    #[test]
    fn test_apply_chunks_simple_replace() {
        let content = "line1\nold_line\nline3\n";
        let chunks = vec![UpdateChunk {
            old_lines: vec!["old_line".into()],
            new_lines: vec!["new_line".into()],
            change_context: None,
            is_end_of_file: false,
        }];
        let result = apply_update_chunks(content, "test.txt", &chunks).unwrap();
        assert!(result.contains("new_line"));
        assert!(!result.contains("old_line"));
    }

    #[test]
    fn test_apply_chunks_add_lines() {
        let content = "aaa\nbbb\nccc\n";
        let chunks = vec![UpdateChunk {
            old_lines: vec!["bbb".into()],
            new_lines: vec!["bbb".into(), "bbb2".into()],
            change_context: None,
            is_end_of_file: false,
        }];
        let result = apply_update_chunks(content, "test.txt", &chunks).unwrap();
        assert!(result.contains("bbb\nbbb2\n"));
    }

    #[test]
    fn test_apply_chunks_remove_lines() {
        let content = "aaa\nbbb\nccc\nddd\n";
        let chunks = vec![UpdateChunk {
            old_lines: vec!["bbb".into(), "ccc".into()],
            new_lines: vec!["bbb".into()],
            change_context: None,
            is_end_of_file: false,
        }];
        let result = apply_update_chunks(content, "test.txt", &chunks).unwrap();
        assert!(result.contains("bbb\nddd"));
        assert!(!result.contains("ccc"));
    }

    #[test]
    fn test_apply_chunks_with_context_seeking() {
        let content = "fn first() {\n    old1();\n}\nfn second() {\n    old2();\n}\n";
        let chunks = vec![UpdateChunk {
            old_lines: vec!["    old2();".into()],
            new_lines: vec!["    new2();".into()],
            change_context: Some("fn second() {".into()),
            is_end_of_file: false,
        }];
        let result = apply_update_chunks(content, "test.rs", &chunks).unwrap();
        assert!(result.contains("old1")); // first function unchanged
        assert!(result.contains("new2")); // second function updated
        assert!(!result.contains("old2"));
    }

    #[test]
    fn test_apply_chunks_eof_anchor() {
        let content = "dup\nmiddle\ndup\n";
        let chunks = vec![UpdateChunk {
            old_lines: vec!["dup".into()],
            new_lines: vec!["changed".into()],
            change_context: None,
            is_end_of_file: true,
        }];
        let result = apply_update_chunks(content, "test.txt", &chunks).unwrap();
        // Should change the LAST "dup", not the first
        assert!(result.starts_with("dup\n")); // first dup preserved
        assert!(result.contains("changed")); // last dup changed
    }

    #[test]
    fn test_apply_chunks_fuzzy_whitespace() {
        let content = "    indented_line    \nnext\n";
        let chunks = vec![UpdateChunk {
            old_lines: vec!["indented_line".into()],
            new_lines: vec!["    new_line".into()],
            change_context: None,
            is_end_of_file: false,
        }];
        // Should match via trim pass even though whitespace differs
        let result = apply_update_chunks(content, "test.txt", &chunks).unwrap();
        assert!(result.contains("new_line"));
    }

    #[test]
    fn test_apply_chunks_multiple_chunks() {
        let content = "aaa\nbbb\nccc\nddd\neee\n";
        let chunks = vec![
            UpdateChunk {
                old_lines: vec!["bbb".into()],
                new_lines: vec!["BBB".into()],
                change_context: None,
                is_end_of_file: false,
            },
            UpdateChunk {
                old_lines: vec!["ddd".into()],
                new_lines: vec!["DDD".into()],
                change_context: None,
                is_end_of_file: false,
            },
        ];
        let result = apply_update_chunks(content, "test.txt", &chunks).unwrap();
        assert!(result.contains("BBB"));
        assert!(result.contains("DDD"));
        assert!(!result.contains("\nbbb\n"));
        assert!(!result.contains("\nddd\n"));
    }

    #[test]
    fn test_apply_chunks_not_found_errors() {
        let content = "aaa\nbbb\n";
        let chunks = vec![UpdateChunk {
            old_lines: vec!["zzz".into()],
            new_lines: vec!["new".into()],
            change_context: None,
            is_end_of_file: false,
        }];
        assert!(apply_update_chunks(content, "test.txt", &chunks).is_err());
    }

    #[test]
    fn test_apply_chunks_pure_addition() {
        let content = "existing\n";
        let chunks = vec![UpdateChunk {
            old_lines: vec![],
            new_lines: vec!["added_line".into()],
            change_context: None,
            is_end_of_file: false,
        }];
        let result = apply_update_chunks(content, "test.txt", &chunks).unwrap();
        assert!(result.contains("added_line"));
    }

    // ── Unified diff parser ──────────────────────────────────────────

    #[test]
    fn test_unified_diff_basic() {
        let patch = "\
--- a/foo.txt
+++ b/foo.txt
@@ -1,3 +1,3 @@
 line1
-old
+new
 line3
";
        let ops = parse_unified_diff(patch).unwrap();
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            FileOp::UnifiedHunk { path, hunks } => {
                assert_eq!(path, "foo.txt");
                assert_eq!(hunks.len(), 1);
            }
            _ => panic!("Expected UnifiedHunk"),
        }
    }

    #[test]
    fn test_unified_diff_multiple_files() {
        let patch = "\
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old_a
+new_a
--- a/b.txt
+++ b/b.txt
@@ -1 +1 @@
-old_b
+new_b
";
        let ops = parse_unified_diff(patch).unwrap();
        assert_eq!(ops.len(), 2);
    }

    #[test]
    fn test_unified_diff_new_file() {
        let patch = "\
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+hello
+world
";
        let ops = parse_unified_diff(patch).unwrap();
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            FileOp::UnifiedHunk { path, hunks } => {
                assert_eq!(path, "new.txt");
                assert_eq!(hunks[0].lines.len(), 2);
            }
            _ => panic!("Expected UnifiedHunk"),
        }
    }

    #[test]
    fn test_unified_diff_no_newline_marker() {
        let patch = "\
--- a/foo.txt
+++ b/foo.txt
@@ -1,2 +1,2 @@
-old
+new
\\ No newline at end of file
";
        let ops = parse_unified_diff(patch).unwrap();
        match &ops[0] {
            FileOp::UnifiedHunk { hunks, .. } => {
                assert_eq!(hunks[0].lines.len(), 2); // Remove + Add only
            }
            _ => panic!("Expected UnifiedHunk"),
        }
    }

    #[test]
    fn test_unified_diff_empty_context_line() {
        let patch = "\
--- a/foo.txt
+++ b/foo.txt
@@ -1,3 +1,3 @@
 line1

-old
+new
";
        let ops = parse_unified_diff(patch).unwrap();
        match &ops[0] {
            FileOp::UnifiedHunk { hunks, .. } => {
                assert_eq!(hunks[0].lines.len(), 4); // context + empty context + remove + add
            }
            _ => panic!("Expected UnifiedHunk"),
        }
    }

    #[test]
    fn test_unified_diff_multiple_hunks() {
        let patch = "\
--- a/foo.txt
+++ b/foo.txt
@@ -1,3 +1,3 @@
 a
-b
+B
 c
@@ -5,3 +5,3 @@
 e
-f
+F
 g
";
        let ops = parse_unified_diff(patch).unwrap();
        match &ops[0] {
            FileOp::UnifiedHunk { hunks, .. } => {
                assert_eq!(hunks.len(), 2);
            }
            _ => panic!("Expected UnifiedHunk"),
        }
    }

    #[test]
    fn test_unified_diff_empty_input() {
        assert!(parse_unified_diff("").is_err());
    }

    #[test]
    fn test_unified_diff_garbage_input() {
        assert!(parse_unified_diff("random text\nno patches here").is_err());
    }

    #[test]
    fn test_unified_diff_with_function_context() {
        let patch = "\
--- a/foo.txt
+++ b/foo.txt
@@ -1,5 +1,5 @@ function context
 line1
 line2
-old
+new
 line4
 line5
";
        let ops = parse_unified_diff(patch).unwrap();
        match &ops[0] {
            FileOp::UnifiedHunk { hunks, .. } => {
                assert_eq!(hunks[0].lines.len(), 6);
            }
            _ => panic!("Expected UnifiedHunk"),
        }
    }

    // ── apply_unified_hunks ──────────────────────────────────────────

    #[test]
    fn test_apply_unified_hunks_basic() {
        let content = "line1\nold\nline3\n";
        let hunks = vec![UnifiedHunk {
            old_start: 1,
            _old_count: 3,
            _new_start: 1,
            _new_count: 3,
            lines: vec![
                DiffLine::Context("line1".into()),
                DiffLine::Remove("old".into()),
                DiffLine::Add("new".into()),
                DiffLine::Context("line3".into()),
            ],
        }];
        let result = apply_unified_hunks(content, &hunks).unwrap();
        assert!(result.contains("new"));
        assert!(!result.contains("old"));
    }

    #[test]
    fn test_apply_unified_hunks_create_file() {
        let hunks = vec![UnifiedHunk {
            old_start: 0,
            _old_count: 0,
            _new_start: 1,
            _new_count: 2,
            lines: vec![DiffLine::Add("hello".into()), DiffLine::Add("world".into())],
        }];
        let result = apply_unified_hunks("", &hunks).unwrap();
        assert!(result.contains("hello"));
        assert!(result.contains("world"));
    }

    #[test]
    fn test_apply_unified_hunks_multiple() {
        let content = "aaa\nbbb\nccc\nddd\neee\nfff\nggg\n";
        let hunks = vec![
            UnifiedHunk {
                old_start: 1,
                _old_count: 3,
                _new_start: 1,
                _new_count: 3,
                lines: vec![
                    DiffLine::Context("aaa".into()),
                    DiffLine::Remove("bbb".into()),
                    DiffLine::Add("BBB".into()),
                    DiffLine::Context("ccc".into()),
                ],
            },
            UnifiedHunk {
                old_start: 5,
                _old_count: 3,
                _new_start: 5,
                _new_count: 3,
                lines: vec![
                    DiffLine::Context("eee".into()),
                    DiffLine::Remove("fff".into()),
                    DiffLine::Add("FFF".into()),
                    DiffLine::Context("ggg".into()),
                ],
            },
        ];
        let result = apply_unified_hunks(content, &hunks).unwrap();
        assert!(result.contains("BBB"));
        assert!(result.contains("FFF"));
        assert!(!result.contains("\nbbb\n"));
        assert!(!result.contains("\nfff\n"));
    }

    #[test]
    fn test_apply_unified_hunks_preserves_trailing_newline() {
        let content = "hello\nworld\n";
        let hunks = vec![UnifiedHunk {
            old_start: 1,
            _old_count: 1,
            _new_start: 1,
            _new_count: 1,
            lines: vec![
                DiffLine::Remove("hello".into()),
                DiffLine::Add("goodbye".into()),
            ],
        }];
        let result = apply_unified_hunks(content, &hunks).unwrap();
        assert!(result.ends_with('\n'));
    }

    // ── parse_hunk_header ────────────────────────────────────────────

    #[test]
    fn test_parse_hunk_header_basic() {
        let (os, oc, ns, nc) = parse_hunk_header("@@ -1,5 +1,7 @@").unwrap();
        assert_eq!((os, oc, ns, nc), (1, 5, 1, 7));
    }

    #[test]
    fn test_parse_hunk_header_single_line() {
        let (os, oc, ns, nc) = parse_hunk_header("@@ -1 +1 @@").unwrap();
        assert_eq!((os, oc, ns, nc), (1, 1, 1, 1));
    }

    #[test]
    fn test_parse_hunk_header_with_context() {
        let (os, oc, ns, nc) = parse_hunk_header("@@ -10,3 +12,5 @@ fn some_function").unwrap();
        assert_eq!((os, oc, ns, nc), (10, 3, 12, 5));
    }

    #[test]
    fn test_parse_hunk_header_invalid() {
        assert!(parse_hunk_header("not a header").is_err());
    }

    // ── parse_range ──────────────────────────────────────────────────

    #[test]
    fn test_parse_range_comma() {
        assert_eq!(parse_range("10,5").unwrap(), (10, 5));
    }

    #[test]
    fn test_parse_range_single() {
        assert_eq!(parse_range("42").unwrap(), (42, 1));
    }

    // ── count_affected_files ─────────────────────────────────────────

    #[test]
    fn test_count_affected_unified() {
        let patch = "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-x\n+y\n--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-x\n+y";
        assert_eq!(count_affected_files(patch), 2);
    }

    #[test]
    fn test_count_affected_opencode() {
        let patch = "\
*** Begin Patch
*** Add File: a.txt
+a
*** Delete File: b.txt
*** Update File: c.txt
@@
-old
+new
*** End Patch";
        assert_eq!(count_affected_files(patch), 3);
    }

    // ── Integration: heredoc + opencode patch ────────────────────────

    #[test]
    fn test_heredoc_wrapped_opencode_patch() {
        let patch = "\
cat <<'EOF'
*** Begin Patch
*** Add File: test.txt
+hello world
*** End Patch
EOF";
        let ops = parse_patch(patch).unwrap();
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], FileOp::Add { path, .. } if path == "test.txt"));
    }

    // ── Filesystem integration tests ─────────────────────────────────

    #[tokio::test]
    async fn test_apply_file_ops_add() {
        let dir = crate::test_utils::tmp_dir("patch_add");
        let target = dir.join("new_file.txt");
        let ops = vec![FileOp::Add {
            path: target.display().to_string(),
            contents: "hello\nworld".into(),
        }];
        let results = apply_file_ops(&ops, "test").await.unwrap();
        assert!(results[0].starts_with("A "));
        assert!(target.exists());
        let content = std::fs::read_to_string(&target).unwrap();
        assert_eq!(content, "hello\nworld");
    }

    #[tokio::test]
    async fn test_apply_file_ops_delete() {
        let dir = crate::test_utils::tmp_dir("patch_del");
        let target = dir.join("to_delete.txt");
        std::fs::write(&target, "content").unwrap();
        assert!(target.exists());

        let ops = vec![FileOp::Delete {
            path: target.display().to_string(),
        }];
        let results = apply_file_ops(&ops, "test").await.unwrap();
        assert!(results[0].starts_with("D "));
        assert!(!target.exists());
    }

    #[tokio::test]
    async fn test_apply_file_ops_update() {
        let dir = crate::test_utils::tmp_dir("patch_upd");
        let target = dir.join("update.txt");
        std::fs::write(&target, "aaa\nold\nccc\n").unwrap();

        let ops = vec![FileOp::Update {
            path: target.display().to_string(),
            move_path: None,
            chunks: vec![UpdateChunk {
                old_lines: vec!["old".into()],
                new_lines: vec!["new".into()],
                change_context: None,
                is_end_of_file: false,
            }],
        }];
        let results = apply_file_ops(&ops, "test").await.unwrap();
        assert!(results[0].starts_with("M "));
        let content = std::fs::read_to_string(&target).unwrap();
        assert!(content.contains("new"));
        assert!(!content.contains("old"));
    }

    #[tokio::test]
    async fn test_apply_file_ops_move() {
        let dir = crate::test_utils::tmp_dir("patch_mv");
        let source = dir.join("old_name.txt");
        let dest = dir.join("new_name.txt");
        std::fs::write(&source, "content\n").unwrap();

        let ops = vec![FileOp::Update {
            path: source.display().to_string(),
            move_path: Some(dest.display().to_string()),
            chunks: vec![UpdateChunk {
                old_lines: vec!["content".into()],
                new_lines: vec!["modified content".into()],
                change_context: None,
                is_end_of_file: false,
            }],
        }];
        let results = apply_file_ops(&ops, "test").await.unwrap();
        assert!(results[0].starts_with("R "));
        assert!(!source.exists());
        assert!(dest.exists());
        let content = std::fs::read_to_string(&dest).unwrap();
        assert!(content.contains("modified content"));
    }

    #[tokio::test]
    async fn test_apply_file_ops_unified_hunk() {
        let dir = crate::test_utils::tmp_dir("patch_uni");
        let target = dir.join("unified.txt");
        std::fs::write(&target, "line1\nold_line\nline3\n").unwrap();

        let ops = vec![FileOp::UnifiedHunk {
            path: target.display().to_string(),
            hunks: vec![UnifiedHunk {
                old_start: 1,
                _old_count: 3,
                _new_start: 1,
                _new_count: 3,
                lines: vec![
                    DiffLine::Context("line1".into()),
                    DiffLine::Remove("old_line".into()),
                    DiffLine::Add("new_line".into()),
                    DiffLine::Context("line3".into()),
                ],
            }],
        }];
        let results = apply_file_ops(&ops, "test").await.unwrap();
        assert!(results[0].contains("M "));
        let content = std::fs::read_to_string(&target).unwrap();
        assert!(content.contains("new_line"));
    }

    #[tokio::test]
    async fn test_apply_file_ops_unified_create() {
        let dir = crate::test_utils::tmp_dir("patch_ucreate");
        let target = dir.join("brand_new.txt");

        let ops = vec![FileOp::UnifiedHunk {
            path: target.display().to_string(),
            hunks: vec![UnifiedHunk {
                old_start: 0,
                _old_count: 0,
                _new_start: 1,
                _new_count: 2,
                lines: vec![DiffLine::Add("hello".into()), DiffLine::Add("world".into())],
            }],
        }];
        let results = apply_file_ops(&ops, "test").await.unwrap();
        assert!(results[0].starts_with("A "));
        assert!(target.exists());
    }

    #[tokio::test]
    async fn test_full_roundtrip_opencode_patch() {
        let dir = crate::test_utils::tmp_dir("patch_roundtrip");
        let target = dir.join("roundtrip.txt");
        std::fs::write(&target, "fn main() {\n    println!(\"old\");\n}\n").unwrap();

        let patch = format!(
            "\
*** Begin Patch
*** Update File: {}
@@
 fn main() {{
-    println!(\"old\");
+    println!(\"new\");
 }}
*** End Patch",
            target.display()
        );

        let ops = parse_patch(&patch).unwrap();
        let results = apply_file_ops(&ops, "test").await.unwrap();
        assert!(results[0].starts_with("M "));

        let content = std::fs::read_to_string(&target).unwrap();
        assert!(content.contains("println!(\"new\")"));
        assert!(!content.contains("println!(\"old\")"));
    }

    #[tokio::test]
    async fn test_full_roundtrip_unified_patch() {
        let dir = crate::test_utils::tmp_dir("patch_uni_rt");
        let target = dir.join("unified_rt.txt");
        std::fs::write(&target, "aaa\nbbb\nccc\n").unwrap();

        let patch = format!(
            "--- a/{path}\n+++ b/{path}\n@@ -1,3 +1,3 @@\n aaa\n-bbb\n+BBB\n ccc\n",
            path = target.display()
        );

        let ops = parse_patch(&patch).unwrap();
        let results = apply_file_ops(&ops, "test").await.unwrap();
        assert!(results[0].contains("M "));

        let content = std::fs::read_to_string(&target).unwrap();
        assert!(content.contains("BBB"));
        assert!(!content.contains("\nbbb\n"));
    }
}
