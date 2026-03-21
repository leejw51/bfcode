//! Context Window Guard — pre-flight checks to prevent context overflow.
//!
//! Inspired by openclaw's `context-window-guard.ts` and
//! `tool-result-context-guard.ts`.  Provides:
//!
//! 1. **Hard floor** — refuse to send if remaining budget < 16 K tokens.
//! 2. **Preemptive overflow** — trigger compaction at 90 % fill.
//! 3. **Tool-result truncation** — cap individual tool results to 30 % of
//!    the context window so one huge `cat` doesn't blow the budget.

use crate::context;
use crate::types::{self, Message};

// ---------------------------------------------------------------------------
// Constants (mirroring openclaw values)
// ---------------------------------------------------------------------------

/// Absolute minimum tokens we require *after* all messages.
/// If the remaining budget is below this, we refuse to send.
pub const CONTEXT_WINDOW_HARD_MIN_TOKENS: u64 = 16_000;

/// Warn (but don't block) if the context window is smaller than this.
pub const CONTEXT_WINDOW_WARN_BELOW_TOKENS: u64 = 32_000;

/// Trigger preemptive compaction when usage exceeds this ratio.
pub const PREEMPTIVE_OVERFLOW_RATIO: f64 = 0.90;

/// Maximum share of the context window a single tool result may occupy.
const MAX_TOOL_RESULT_CONTEXT_SHARE: f64 = 0.30;

/// Absolute hard cap on tool result size (chars ≈ tokens × 4).
pub const HARD_MAX_TOOL_RESULT_CHARS: usize = 400_000;

/// Minimum chars we always keep even after truncation.
const MIN_KEEP_CHARS: usize = 2_000;

/// Suffix appended to truncated tool results.
const TRUNCATION_SUFFIX: &str = "\n[Content truncated — original was too large for the model's context window. \
     Consider reading specific sections or using grep to find relevant content.]";

// ---------------------------------------------------------------------------
// Context Window Info
// ---------------------------------------------------------------------------

/// Result of a pre-flight context check.
#[derive(Debug)]
pub struct ContextWindowCheck {
    /// Estimated token usage of current conversation.
    pub estimated_tokens: u64,
    /// Resolved context limit for the active model.
    pub context_limit: u64,
    /// Remaining token budget.
    pub remaining: u64,
    /// Whether compaction is recommended.
    pub needs_compaction: bool,
    /// Whether the request should be blocked (hard floor breached).
    pub blocked: bool,
    /// Human-readable status.
    pub status: ContextStatus,
}

#[derive(Debug, PartialEq)]
pub enum ContextStatus {
    /// Plenty of room.
    Ok,
    /// Above 80% — auto-compaction zone (existing behavior).
    Warning,
    /// Above 90% — preemptive compaction required NOW.
    PreemptiveOverflow,
    /// Remaining tokens below hard floor — refuse to send.
    Blocked,
}

impl std::fmt::Display for ContextStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ok => write!(f, "ok"),
            Self::Warning => write!(f, "warning"),
            Self::PreemptiveOverflow => write!(f, "preemptive_overflow"),
            Self::Blocked => write!(f, "blocked"),
        }
    }
}

// ---------------------------------------------------------------------------
// Pre-flight check
// ---------------------------------------------------------------------------

/// Run a pre-flight context-window check.
///
/// Call this *before* sending messages to the LLM.  The caller should:
/// - If `blocked`, refuse to send and compact first.
/// - If `needs_compaction`, compact before sending.
pub fn check_context_window(messages: &[Message], model: &str) -> ContextWindowCheck {
    let estimated = context::estimate_conversation_tokens(messages);
    let limit = types::context_limit_for_model(model);

    if limit == 0 {
        // Unknown model — cannot guard, allow through
        return ContextWindowCheck {
            estimated_tokens: estimated,
            context_limit: 0,
            remaining: 0,
            needs_compaction: false,
            blocked: false,
            status: ContextStatus::Ok,
        };
    }

    let remaining = limit.saturating_sub(estimated);
    let usage_ratio = estimated as f64 / limit as f64;

    let (status, needs_compaction, blocked) = if remaining < CONTEXT_WINDOW_HARD_MIN_TOKENS {
        (ContextStatus::Blocked, true, true)
    } else if usage_ratio >= PREEMPTIVE_OVERFLOW_RATIO {
        (ContextStatus::PreemptiveOverflow, true, false)
    } else if usage_ratio >= 0.80 {
        (ContextStatus::Warning, true, false)
    } else {
        (ContextStatus::Ok, false, false)
    };

    ContextWindowCheck {
        estimated_tokens: estimated,
        context_limit: limit,
        remaining,
        needs_compaction,
        blocked,
        status,
    }
}

// ---------------------------------------------------------------------------
// Tool result truncation
// ---------------------------------------------------------------------------

/// Calculate the maximum allowed characters for a tool result given the
/// model's context window.
pub fn max_tool_result_chars(model: &str) -> usize {
    let limit = types::context_limit_for_model(model);
    if limit == 0 {
        return HARD_MAX_TOOL_RESULT_CHARS;
    }
    let max_tokens = (limit as f64 * MAX_TOOL_RESULT_CONTEXT_SHARE) as usize;
    let max_chars = max_tokens * 4; // ~4 chars per token
    max_chars.min(HARD_MAX_TOOL_RESULT_CHARS)
}

/// Truncate a tool result if it exceeds the allowed budget.
///
/// Uses a head+tail strategy: keeps 70 % from the start and 30 % from
/// the end (the tail often contains error messages or summaries).
pub fn truncate_tool_result(result: &str, model: &str) -> String {
    let max_chars = max_tool_result_chars(model);
    if result.len() <= max_chars {
        return result.to_string();
    }

    // Check if the result is small enough to keep entirely
    if result.len() <= MIN_KEEP_CHARS {
        return result.to_string();
    }

    let budget = max_chars.saturating_sub(TRUNCATION_SUFFIX.len());
    if budget < MIN_KEEP_CHARS {
        // Budget too small — just take head
        let head: String = result.chars().take(budget).collect();
        return format!("{head}{TRUNCATION_SUFFIX}");
    }

    // Head+tail split: 70/30
    let head_budget = budget * 70 / 100;
    let tail_budget = budget - head_budget;

    // Find clean cut points at newline boundaries
    let head = take_head(result, head_budget);
    let tail = take_tail(result, tail_budget);

    let original_size = result.len();
    format!("{head}\n\n[...truncated {original_size} chars total...]\n\n{tail}{TRUNCATION_SUFFIX}")
}

/// Take up to `budget` chars from the start, cutting at a newline if possible.
fn take_head(s: &str, budget: usize) -> &str {
    if s.len() <= budget {
        return s;
    }
    let slice = &s[..budget];
    // Try to cut at last newline within budget
    if let Some(pos) = slice.rfind('\n') {
        &s[..pos]
    } else {
        slice
    }
}

/// Take up to `budget` chars from the end, cutting at a newline if possible.
fn take_tail(s: &str, budget: usize) -> &str {
    if s.len() <= budget {
        return s;
    }
    let start = s.len() - budget;
    let slice = &s[start..];
    // Try to cut at first newline within budget
    if let Some(pos) = slice.find('\n') {
        &slice[pos + 1..]
    } else {
        slice
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── check_context_window tests ───────────────────────────────────────

    #[test]
    fn test_check_context_ok() {
        let messages = vec![Message::system("hello"), Message::user("hi")];
        let check = check_context_window(&messages, "grok-4-1-fast");
        assert_eq!(check.status, ContextStatus::Ok);
        assert!(!check.blocked);
        assert!(!check.needs_compaction);
        assert!(check.remaining > 0);
        assert!(check.context_limit > 0);
    }

    #[test]
    fn test_check_context_warning_zone() {
        // grok-4-1-fast: 131K limit.  80% = 104,857 tokens → ~419K chars
        let big = "x".repeat(420_000);
        let messages = vec![Message::system(&big)];
        let check = check_context_window(&messages, "grok-4-1-fast");
        // Should be Warning or PreemptiveOverflow
        assert!(
            check.status == ContextStatus::Warning
                || check.status == ContextStatus::PreemptiveOverflow
        );
        assert!(check.needs_compaction);
        assert!(!check.blocked);
    }

    #[test]
    fn test_check_context_preemptive_overflow() {
        // 90% of 131K = 117,964 tokens → ~472K chars
        let big = "x".repeat(475_000);
        let messages = vec![Message::system(&big)];
        let check = check_context_window(&messages, "grok-4-1-fast");
        assert!(
            check.status == ContextStatus::PreemptiveOverflow
                || check.status == ContextStatus::Blocked
        );
        assert!(check.needs_compaction);
    }

    #[test]
    fn test_check_context_blocked() {
        // Exceed limit: 131K tokens → ~524K chars
        let big = "x".repeat(520_000);
        let messages = vec![Message::system(&big)];
        let check = check_context_window(&messages, "grok-4-1-fast");
        assert_eq!(check.status, ContextStatus::Blocked);
        assert!(check.blocked);
        assert!(check.needs_compaction);
    }

    #[test]
    fn test_check_unknown_model_passes() {
        let messages = vec![Message::user("hello")];
        let check = check_context_window(&messages, "unknown-model-xyz");
        // Unknown models get default limit, should still work
        assert!(!check.blocked);
    }

    #[test]
    fn test_check_empty_conversation() {
        let messages: Vec<Message> = vec![];
        let check = check_context_window(&messages, "grok-4-1-fast");
        assert_eq!(check.status, ContextStatus::Ok);
        assert_eq!(check.estimated_tokens, 0);
    }

    #[test]
    fn test_check_multiple_providers() {
        let messages = vec![Message::user("hello")];

        let grok = check_context_window(&messages, "grok-4-1-fast");
        let openai = check_context_window(&messages, "gpt-4o");
        let claude = check_context_window(&messages, "claude-sonnet-4-20250514");

        // All should be OK with tiny input
        assert_eq!(grok.status, ContextStatus::Ok);
        assert_eq!(openai.status, ContextStatus::Ok);
        assert_eq!(claude.status, ContextStatus::Ok);

        // Claude has largest context window
        assert!(claude.context_limit >= grok.context_limit);
    }

    // ── ContextStatus Display tests ──────────────────────────────────────

    #[test]
    fn test_context_status_display() {
        assert_eq!(format!("{}", ContextStatus::Ok), "ok");
        assert_eq!(format!("{}", ContextStatus::Warning), "warning");
        assert_eq!(
            format!("{}", ContextStatus::PreemptiveOverflow),
            "preemptive_overflow"
        );
        assert_eq!(format!("{}", ContextStatus::Blocked), "blocked");
    }

    // ── max_tool_result_chars tests ──────────────────────────────────────

    #[test]
    fn test_max_tool_result_chars_grok() {
        let max = max_tool_result_chars("grok-4-1-fast");
        // 131072 * 0.30 * 4 = 157,286
        assert!(max > 100_000);
        assert!(max <= HARD_MAX_TOOL_RESULT_CHARS);
    }

    #[test]
    fn test_max_tool_result_chars_anthropic() {
        let max = max_tool_result_chars("claude-sonnet-4-20250514");
        // 200000 * 0.30 * 4 = 240,000
        assert!(max > 200_000);
        assert!(max <= HARD_MAX_TOOL_RESULT_CHARS);
    }

    #[test]
    fn test_max_tool_result_chars_hard_cap() {
        // Even with a huge context window, should not exceed hard cap
        // (the hard cap is 400K, so most models won't hit it)
        let max = max_tool_result_chars("gpt-4o");
        assert!(max <= HARD_MAX_TOOL_RESULT_CHARS);
    }

    // ── truncate_tool_result tests ───────────────────────────────────────

    #[test]
    fn test_truncate_small_result_unchanged() {
        let result = "short result";
        let truncated = truncate_tool_result(result, "grok-4-1-fast");
        assert_eq!(truncated, result);
    }

    #[test]
    fn test_truncate_empty_result() {
        let truncated = truncate_tool_result("", "grok-4-1-fast");
        assert_eq!(truncated, "");
    }

    #[test]
    fn test_truncate_large_result() {
        let large = "x".repeat(200_000);
        let truncated = truncate_tool_result(&large, "grok-4-1-fast");
        assert!(truncated.len() < large.len());
        assert!(truncated.contains("truncated"));
    }

    #[test]
    fn test_truncate_preserves_head_and_tail() {
        let mut content = String::new();
        content.push_str("HEADER_START\n");
        content.push_str(&"x".repeat(200_000));
        content.push_str("\nTAILER_END");

        let truncated = truncate_tool_result(&content, "grok-4-1-fast");
        assert!(truncated.contains("HEADER_START"));
        assert!(truncated.contains("TAILER_END"));
    }

    #[test]
    fn test_truncate_includes_suffix() {
        let large = "x\n".repeat(200_000);
        let truncated = truncate_tool_result(&large, "grok-4-1-fast");
        assert!(truncated.contains("Content truncated"));
        assert!(truncated.contains("context window"));
    }

    #[test]
    fn test_truncate_respects_model_context() {
        // Smaller context window model should truncate more aggressively
        let large = "x".repeat(200_000);
        let grok_truncated = truncate_tool_result(&large, "grok-4-1-fast"); // 131K
        let claude_truncated = truncate_tool_result(&large, "claude-sonnet-4-20250514"); // 200K

        // Claude allows more content
        assert!(claude_truncated.len() >= grok_truncated.len());
    }

    // ── take_head tests ──────────────────────────────────────────────────

    #[test]
    fn test_take_head_fits() {
        let s = "hello world";
        assert_eq!(take_head(s, 100), s);
    }

    #[test]
    fn test_take_head_cuts_at_newline() {
        let s = "line1\nline2\nline3";
        let head = take_head(s, 8);
        assert_eq!(head, "line1"); // cuts at newline within budget
    }

    #[test]
    fn test_take_head_no_newline() {
        let s = "abcdefghij";
        let head = take_head(s, 5);
        assert_eq!(head, "abcde");
    }

    // ── take_tail tests ──────────────────────────────────────────────────

    #[test]
    fn test_take_tail_fits() {
        let s = "hello world";
        assert_eq!(take_tail(s, 100), s);
    }

    #[test]
    fn test_take_tail_cuts_at_newline() {
        let s = "line1\nline2\nline3\nline4\nline5";
        let tail = take_tail(s, 12);
        // Should start at a newline boundary
        assert!(tail.starts_with("line4") || tail.starts_with("line5"));
    }

    #[test]
    fn test_take_tail_no_newline() {
        let s = "abcdefghij";
        let tail = take_tail(s, 5);
        assert_eq!(tail, "fghij");
    }

    // ── Constants sanity checks ──────────────────────────────────────────

    #[test]
    fn test_constants_are_reasonable() {
        assert!(CONTEXT_WINDOW_HARD_MIN_TOKENS > 0);
        assert!(CONTEXT_WINDOW_WARN_BELOW_TOKENS > CONTEXT_WINDOW_HARD_MIN_TOKENS);
        assert!(PREEMPTIVE_OVERFLOW_RATIO > 0.5);
        assert!(PREEMPTIVE_OVERFLOW_RATIO < 1.0);
        assert!(HARD_MAX_TOOL_RESULT_CHARS > MIN_KEEP_CHARS);
    }

    // ── Remaining budget calculation ─────────────────────────────────────

    #[test]
    fn test_remaining_budget_accurate() {
        let messages = vec![Message::system("sys prompt"), Message::user("hello world")];
        let check = check_context_window(&messages, "grok-4-1-fast");
        assert_eq!(
            check.remaining,
            check.context_limit - check.estimated_tokens
        );
    }

    // ── Edge: conversation with tool results ─────────────────────────────

    #[test]
    fn test_check_with_tool_results() {
        let messages = vec![
            Message::system("sys"),
            Message::user("read file"),
            Message::assistant_text("sure"),
            Message::tool_result("tc_1", &"x".repeat(10_000)),
        ];
        let check = check_context_window(&messages, "grok-4-1-fast");
        // Should account for tool result tokens
        assert!(check.estimated_tokens > 2_500); // 10K chars ≈ 2.5K tokens
    }
}
