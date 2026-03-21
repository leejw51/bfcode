//! Fallback Chain — cascading model failover on rate-limit / outage.
//!
//! Inspired by openclaw's `model-fallback.ts`: when the primary model
//! hits a 429, 5xx, or timeout after exhausting retries, the chain
//! transparently tries the next candidate.  Cooldown tracking prevents
//! hammering a provider that is already down.

use crate::api::{self, ChatClient};
use crate::types::*;
use anyhow::{Result, bail};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Cooldown tracking
// ---------------------------------------------------------------------------

/// Minimum seconds before we probe a cooled-down model again.
const MIN_COOLDOWN_SECS: u64 = 30;

/// Maximum cooldown duration (5 minutes).
const MAX_COOLDOWN_SECS: u64 = 300;

/// Backoff factor for successive failures.
const COOLDOWN_BACKOFF_FACTOR: u64 = 2;

#[derive(Debug)]
struct CooldownEntry {
    until: Instant,
    failures: u32,
    reason: FailoverReason,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FailoverReason {
    RateLimit,
    Overloaded,
    Timeout,
    Auth,
    Unknown,
}

impl std::fmt::Display for FailoverReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RateLimit => write!(f, "rate_limit"),
            Self::Overloaded => write!(f, "overloaded"),
            Self::Timeout => write!(f, "timeout"),
            Self::Auth => write!(f, "auth"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

/// Classify an error string into a failover reason.
fn classify_error(err_msg: &str) -> FailoverReason {
    let lower = err_msg.to_lowercase();
    if lower.contains("429") || lower.contains("rate limit") || lower.contains("rate_limit") {
        FailoverReason::RateLimit
    } else if lower.contains("503") || lower.contains("overloaded") || lower.contains("capacity") {
        FailoverReason::Overloaded
    } else if lower.contains("timeout") || lower.contains("timed out") {
        FailoverReason::Timeout
    } else if lower.contains("401") || lower.contains("403") || lower.contains("auth") {
        FailoverReason::Auth
    } else {
        FailoverReason::Unknown
    }
}

/// Returns true if the error looks like a context-overflow (should NOT failover).
fn is_context_overflow(err_msg: &str) -> bool {
    let lower = err_msg.to_lowercase();
    lower.contains("context length")
        || lower.contains("context_length")
        || lower.contains("maximum context")
        || lower.contains("token limit")
        || lower.contains("too many tokens")
        || lower.contains("context window")
}

// ---------------------------------------------------------------------------
// FallbackChain client
// ---------------------------------------------------------------------------

/// A ChatClient wrapper that tries a chain of models in order.
///
/// Each candidate is a `(model_name, Box<dyn ChatClient>)`.  The first
/// candidate is the primary; the rest are fallbacks.  On retryable errors
/// (after the inner client's own retries are exhausted), the chain advances.
pub struct FallbackChain {
    candidates: Vec<FallbackCandidate>,
    cooldowns: Mutex<HashMap<String, CooldownEntry>>,
}

struct FallbackCandidate {
    model: String,
    client: Box<dyn ChatClient>,
}

impl FallbackChain {
    /// Build a fallback chain from a primary model + list of fallback model names.
    ///
    /// Each model name is resolved to a provider and a client is created.
    /// Models whose provider keys are missing are silently skipped.
    pub fn build(primary_model: &str, fallback_models: &[String]) -> Result<Self> {
        let mut candidates = Vec::new();

        // Primary — must succeed
        let primary_config = GlobalConfig {
            model: primary_model.to_string(),
            ..GlobalConfig::default()
        };
        let client = api::create_client(&primary_config)?;
        candidates.push(FallbackCandidate {
            model: primary_model.to_string(),
            client,
        });

        // Fallbacks — best-effort
        for fb_model in fallback_models {
            let fb_config = GlobalConfig {
                model: fb_model.clone(),
                ..GlobalConfig::default()
            };
            match api::create_client(&fb_config) {
                Ok(client) => {
                    candidates.push(FallbackCandidate {
                        model: fb_model.clone(),
                        client,
                    });
                }
                Err(e) => {
                    eprintln!(
                        "  {} Fallback model {} unavailable: {}",
                        "!".yellow(),
                        fb_model,
                        e
                    );
                }
            }
        }

        Ok(Self {
            candidates,
            cooldowns: Mutex::new(HashMap::new()),
        })
    }

    /// Record a failure and put the model on cooldown.
    fn record_failure(&self, model: &str, reason: FailoverReason) {
        let mut cd = self.cooldowns.lock().unwrap();
        let entry = cd.entry(model.to_string()).or_insert(CooldownEntry {
            until: Instant::now(),
            failures: 0,
            reason: FailoverReason::Unknown,
        });
        entry.failures += 1;
        let backoff_secs = (MIN_COOLDOWN_SECS
            * COOLDOWN_BACKOFF_FACTOR.pow(entry.failures.min(5) - 1))
        .min(MAX_COOLDOWN_SECS);
        entry.until = Instant::now() + Duration::from_secs(backoff_secs);
        entry.reason = reason.clone();
        eprintln!(
            "  {} Model {} on cooldown for {}s (reason: {})",
            "!".yellow(),
            model,
            backoff_secs,
            reason,
        );
    }

    /// Check if a model is currently in cooldown.
    fn is_cooled_down(&self, model: &str) -> bool {
        let cd = self.cooldowns.lock().unwrap();
        cd.get(model)
            .map(|e| Instant::now() < e.until)
            .unwrap_or(false)
    }

    /// Clear cooldown for a model (on success).
    fn clear_cooldown(&self, model: &str) {
        let mut cd = self.cooldowns.lock().unwrap();
        cd.remove(model);
    }

    /// Get the model name that will actually be used (first non-cooled-down).
    pub fn active_model(&self) -> &str {
        for c in &self.candidates {
            if !self.is_cooled_down(&c.model) {
                return &c.model;
            }
        }
        // All cooled down — try primary anyway
        &self.candidates[0].model
    }

    /// Number of candidates (including primary).
    pub fn candidate_count(&self) -> usize {
        self.candidates.len()
    }
}

use colored::Colorize;

#[async_trait::async_trait]
impl ChatClient for FallbackChain {
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        temperature: f64,
    ) -> Result<ChatResponse> {
        let mut last_err = String::new();

        for candidate in &self.candidates {
            if self.is_cooled_down(&candidate.model) {
                continue;
            }

            match candidate
                .client
                .chat(messages, tools, &candidate.model, temperature)
                .await
            {
                Ok(resp) => {
                    self.clear_cooldown(&candidate.model);
                    if candidate.model != model {
                        eprintln!(
                            "  {} Using fallback model: {}",
                            "↻".yellow().bold(),
                            candidate.model.cyan()
                        );
                    }
                    return Ok(resp);
                }
                Err(e) => {
                    let err_msg = format!("{e}");
                    if is_context_overflow(&err_msg) {
                        return Err(e); // Don't failover on context overflow
                    }
                    let reason = classify_error(&err_msg);
                    if reason == FailoverReason::Auth {
                        // Auth errors are persistent — skip but don't retry
                        eprintln!("  {} {} auth failed, skipping", "✗".red(), candidate.model);
                        self.record_failure(&candidate.model, reason);
                        last_err = err_msg;
                        continue;
                    }
                    self.record_failure(&candidate.model, reason);
                    last_err = err_msg;
                }
            }
        }

        bail!("All models failed. Last error: {last_err}")
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        temperature: f64,
        tx: tokio::sync::mpsc::UnboundedSender<StreamChunk>,
    ) -> Result<ChatResponse> {
        let mut last_err = String::new();

        for (i, candidate) in self.candidates.iter().enumerate() {
            if self.is_cooled_down(&candidate.model) {
                continue;
            }

            // For fallback attempts after the first, we need a fresh channel
            // because the previous tx may have sent partial data.
            // The caller's tx is used only for the first successful attempt.
            let use_tx = if i == 0 || last_err.is_empty() {
                tx.clone()
            } else {
                // Create a throw-away channel for probing
                let (probe_tx, _probe_rx) = tokio::sync::mpsc::unbounded_channel();
                probe_tx
            };

            match candidate
                .client
                .chat_stream(messages, tools, &candidate.model, temperature, use_tx)
                .await
            {
                Ok(resp) => {
                    self.clear_cooldown(&candidate.model);
                    if candidate.model != model {
                        eprintln!(
                            "  {} Using fallback model: {}",
                            "↻".yellow().bold(),
                            candidate.model.cyan()
                        );
                    }
                    // If we used a probe channel, re-stream via the real tx
                    if i > 0 && !last_err.is_empty() {
                        // Send the accumulated text through the real channel
                        if let Some(choice) = resp.choices.first() {
                            if let Some(text) = &choice.message.content {
                                let _ = tx.send(StreamChunk::Text(text.clone()));
                            }
                        }
                        let _ = tx.send(StreamChunk::Done);
                    }
                    return Ok(resp);
                }
                Err(e) => {
                    let err_msg = format!("{e}");
                    if is_context_overflow(&err_msg) {
                        return Err(e);
                    }
                    let reason = classify_error(&err_msg);
                    if reason == FailoverReason::Auth {
                        eprintln!("  {} {} auth failed, skipping", "✗".red(), candidate.model);
                    }
                    self.record_failure(&candidate.model, reason);
                    last_err = err_msg;
                }
            }
        }

        bail!("All models failed. Last error: {last_err}")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ChatClient;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ── Helper: mock client that fails N times then succeeds ──────────────

    struct FailThenSucceedClient {
        fail_count: AtomicUsize,
        fail_msg: String,
        model_name: String,
    }

    impl FailThenSucceedClient {
        fn new(fails: usize, fail_msg: &str, model: &str) -> Self {
            Self {
                fail_count: AtomicUsize::new(fails),
                fail_msg: fail_msg.to_string(),
                model_name: model.to_string(),
            }
        }
    }

    #[async_trait::async_trait]
    impl ChatClient for FailThenSucceedClient {
        async fn chat(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _model: &str,
            _temperature: f64,
        ) -> Result<ChatResponse> {
            let remaining = self.fail_count.fetch_sub(1, Ordering::SeqCst);
            if remaining > 0 {
                bail!("{}", self.fail_msg);
            }
            Ok(ChatResponse {
                choices: vec![Choice {
                    message: Message::assistant_text(&format!("reply from {}", self.model_name)),
                    finish_reason: Some("stop".into()),
                }],
                usage: Some(Usage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                }),
            })
        }

        async fn chat_stream(
            &self,
            messages: &[Message],
            tools: &[ToolDefinition],
            model: &str,
            temperature: f64,
            tx: tokio::sync::mpsc::UnboundedSender<StreamChunk>,
        ) -> Result<ChatResponse> {
            let resp = self.chat(messages, tools, model, temperature).await?;
            if let Some(choice) = resp.choices.first() {
                if let Some(text) = &choice.message.content {
                    let _ = tx.send(StreamChunk::Text(text.clone()));
                }
            }
            let _ = tx.send(StreamChunk::Done);
            Ok(resp)
        }
    }

    /// Always-fail client
    struct AlwaysFailClient {
        fail_msg: String,
    }

    #[async_trait::async_trait]
    impl ChatClient for AlwaysFailClient {
        async fn chat(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _model: &str,
            _temperature: f64,
        ) -> Result<ChatResponse> {
            bail!("{}", self.fail_msg);
        }

        async fn chat_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _model: &str,
            _temperature: f64,
            _tx: tokio::sync::mpsc::UnboundedSender<StreamChunk>,
        ) -> Result<ChatResponse> {
            bail!("{}", self.fail_msg);
        }
    }

    fn make_chain(candidates: Vec<(&str, Box<dyn ChatClient>)>) -> FallbackChain {
        FallbackChain {
            candidates: candidates
                .into_iter()
                .map(|(model, client)| FallbackCandidate {
                    model: model.to_string(),
                    client,
                })
                .collect(),
            cooldowns: Mutex::new(HashMap::new()),
        }
    }

    // ── classify_error tests ─────────────────────────────────────────────

    #[test]
    fn test_classify_rate_limit() {
        assert_eq!(
            classify_error("API error 429: rate limited"),
            FailoverReason::RateLimit
        );
        assert_eq!(
            classify_error("rate_limit exceeded"),
            FailoverReason::RateLimit
        );
    }

    #[test]
    fn test_classify_overloaded() {
        assert_eq!(
            classify_error("API error 503: service overloaded"),
            FailoverReason::Overloaded
        );
        assert_eq!(
            classify_error("server at capacity"),
            FailoverReason::Overloaded
        );
    }

    #[test]
    fn test_classify_timeout() {
        assert_eq!(classify_error("request timed out"), FailoverReason::Timeout);
        assert_eq!(
            classify_error("connection timeout"),
            FailoverReason::Timeout
        );
    }

    #[test]
    fn test_classify_auth() {
        assert_eq!(
            classify_error("API error 401: unauthorized"),
            FailoverReason::Auth
        );
        assert_eq!(
            classify_error("API error 403: forbidden"),
            FailoverReason::Auth
        );
        assert_eq!(
            classify_error("authentication failed"),
            FailoverReason::Auth
        );
    }

    #[test]
    fn test_classify_unknown() {
        assert_eq!(
            classify_error("something went wrong"),
            FailoverReason::Unknown
        );
    }

    // ── is_context_overflow tests ────────────────────────────────────────

    #[test]
    fn test_context_overflow_variants() {
        assert!(is_context_overflow("maximum context length exceeded"));
        assert!(is_context_overflow("context_length_exceeded error"));
        assert!(is_context_overflow("too many tokens in request"));
        assert!(is_context_overflow("exceeds the context window"));
        assert!(is_context_overflow("token limit reached"));
    }

    #[test]
    fn test_not_context_overflow() {
        assert!(!is_context_overflow("API error 429: rate limited"));
        assert!(!is_context_overflow("network error"));
        assert!(!is_context_overflow("timeout"));
    }

    // ── Cooldown tracking tests ──────────────────────────────────────────

    #[test]
    fn test_cooldown_basic() {
        let chain = make_chain(vec![]);
        assert!(!chain.is_cooled_down("test-model"));

        chain.record_failure("test-model", FailoverReason::RateLimit);
        assert!(chain.is_cooled_down("test-model"));

        chain.clear_cooldown("test-model");
        assert!(!chain.is_cooled_down("test-model"));
    }

    #[test]
    fn test_cooldown_multiple_models() {
        let chain = make_chain(vec![]);
        chain.record_failure("model-a", FailoverReason::RateLimit);
        chain.record_failure("model-b", FailoverReason::Overloaded);

        assert!(chain.is_cooled_down("model-a"));
        assert!(chain.is_cooled_down("model-b"));
        assert!(!chain.is_cooled_down("model-c"));

        chain.clear_cooldown("model-a");
        assert!(!chain.is_cooled_down("model-a"));
        assert!(chain.is_cooled_down("model-b"));
    }

    #[test]
    fn test_cooldown_escalation() {
        let chain = make_chain(vec![]);
        // First failure → 30s cooldown
        chain.record_failure("model-a", FailoverReason::RateLimit);
        let cd = chain.cooldowns.lock().unwrap();
        assert_eq!(cd.get("model-a").unwrap().failures, 1);
        drop(cd);

        // Second failure → 60s cooldown (2x backoff)
        chain.record_failure("model-a", FailoverReason::RateLimit);
        let cd = chain.cooldowns.lock().unwrap();
        assert_eq!(cd.get("model-a").unwrap().failures, 2);
    }

    // ── active_model tests ───────────────────────────────────────────────

    #[test]
    fn test_active_model_primary() {
        let chain = make_chain(vec![
            (
                "primary",
                Box::new(AlwaysFailClient {
                    fail_msg: "fail".into(),
                }),
            ),
            (
                "fallback",
                Box::new(AlwaysFailClient {
                    fail_msg: "fail".into(),
                }),
            ),
        ]);
        assert_eq!(chain.active_model(), "primary");
    }

    #[test]
    fn test_active_model_after_cooldown() {
        let chain = make_chain(vec![
            (
                "primary",
                Box::new(AlwaysFailClient {
                    fail_msg: "fail".into(),
                }),
            ),
            (
                "fallback",
                Box::new(AlwaysFailClient {
                    fail_msg: "fail".into(),
                }),
            ),
        ]);
        chain.record_failure("primary", FailoverReason::RateLimit);
        assert_eq!(chain.active_model(), "fallback");
    }

    #[test]
    fn test_active_model_all_cooled_returns_primary() {
        let chain = make_chain(vec![
            (
                "primary",
                Box::new(AlwaysFailClient {
                    fail_msg: "fail".into(),
                }),
            ),
            (
                "fallback",
                Box::new(AlwaysFailClient {
                    fail_msg: "fail".into(),
                }),
            ),
        ]);
        chain.record_failure("primary", FailoverReason::RateLimit);
        chain.record_failure("fallback", FailoverReason::RateLimit);
        // Falls back to primary when all cooled down
        assert_eq!(chain.active_model(), "primary");
    }

    // ── Fallback chain chat tests ────────────────────────────────────────

    #[tokio::test]
    async fn test_primary_succeeds_no_fallback() {
        let chain = make_chain(vec![
            (
                "primary",
                Box::new(FailThenSucceedClient::new(0, "", "primary")),
            ),
            (
                "fallback",
                Box::new(FailThenSucceedClient::new(0, "", "fallback")),
            ),
        ]);

        let msgs = vec![Message::user("hello")];
        let resp = chain.chat(&msgs, &[], "primary", 0.7).await.unwrap();
        let text = resp.choices[0].message.content.as_deref().unwrap();
        assert!(text.contains("primary"));
    }

    #[tokio::test]
    async fn test_fallback_on_rate_limit() {
        let chain = make_chain(vec![
            (
                "primary",
                Box::new(AlwaysFailClient {
                    fail_msg: "API error 429: rate limited".into(),
                }),
            ),
            (
                "fallback",
                Box::new(FailThenSucceedClient::new(0, "", "fallback")),
            ),
        ]);

        let msgs = vec![Message::user("hello")];
        let resp = chain.chat(&msgs, &[], "primary", 0.7).await.unwrap();
        let text = resp.choices[0].message.content.as_deref().unwrap();
        assert!(text.contains("fallback"));
    }

    #[tokio::test]
    async fn test_fallback_on_503() {
        let chain = make_chain(vec![
            (
                "primary",
                Box::new(AlwaysFailClient {
                    fail_msg: "API error 503: overloaded".into(),
                }),
            ),
            (
                "fallback",
                Box::new(FailThenSucceedClient::new(0, "", "fallback")),
            ),
        ]);

        let msgs = vec![Message::user("hello")];
        let resp = chain.chat(&msgs, &[], "primary", 0.7).await.unwrap();
        let text = resp.choices[0].message.content.as_deref().unwrap();
        assert!(text.contains("fallback"));
    }

    #[tokio::test]
    async fn test_no_fallback_on_context_overflow() {
        let chain = make_chain(vec![
            (
                "primary",
                Box::new(AlwaysFailClient {
                    fail_msg: "maximum context length exceeded".into(),
                }),
            ),
            (
                "fallback",
                Box::new(FailThenSucceedClient::new(0, "", "fallback")),
            ),
        ]);

        let msgs = vec![Message::user("hello")];
        let result = chain.chat(&msgs, &[], "primary", 0.7).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("context length"));
    }

    #[tokio::test]
    async fn test_all_models_fail() {
        let chain = make_chain(vec![
            (
                "primary",
                Box::new(AlwaysFailClient {
                    fail_msg: "API error 429: rate limited".into(),
                }),
            ),
            (
                "fallback",
                Box::new(AlwaysFailClient {
                    fail_msg: "API error 503: overloaded".into(),
                }),
            ),
        ]);

        let msgs = vec![Message::user("hello")];
        let result = chain.chat(&msgs, &[], "primary", 0.7).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("All models failed")
        );
    }

    #[tokio::test]
    async fn test_auth_error_skips_candidate() {
        let chain = make_chain(vec![
            (
                "primary",
                Box::new(AlwaysFailClient {
                    fail_msg: "API error 401: unauthorized".into(),
                }),
            ),
            (
                "fallback",
                Box::new(FailThenSucceedClient::new(0, "", "fallback")),
            ),
        ]);

        let msgs = vec![Message::user("hello")];
        let resp = chain.chat(&msgs, &[], "primary", 0.7).await.unwrap();
        let text = resp.choices[0].message.content.as_deref().unwrap();
        assert!(text.contains("fallback"));
        // Primary should be cooled down
        assert!(chain.is_cooled_down("primary"));
    }

    #[tokio::test]
    async fn test_stream_fallback() {
        let chain = make_chain(vec![
            (
                "primary",
                Box::new(AlwaysFailClient {
                    fail_msg: "API error 429: rate limited".into(),
                }),
            ),
            (
                "fallback",
                Box::new(FailThenSucceedClient::new(0, "", "fallback")),
            ),
        ]);

        let msgs = vec![Message::user("hello")];
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let resp = chain
            .chat_stream(&msgs, &[], "primary", 0.7, tx)
            .await
            .unwrap();
        let text = resp.choices[0].message.content.as_deref().unwrap();
        assert!(text.contains("fallback"));

        // Should have received stream chunks
        let mut received = Vec::new();
        while let Ok(chunk) = rx.try_recv() {
            received.push(chunk);
        }
        assert!(!received.is_empty());
    }

    #[tokio::test]
    async fn test_stream_no_fallback_on_context_overflow() {
        let chain = make_chain(vec![
            (
                "primary",
                Box::new(AlwaysFailClient {
                    fail_msg: "too many tokens in request".into(),
                }),
            ),
            (
                "fallback",
                Box::new(FailThenSucceedClient::new(0, "", "fallback")),
            ),
        ]);

        let msgs = vec![Message::user("hello")];
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let result = chain.chat_stream(&msgs, &[], "primary", 0.7, tx).await;
        assert!(result.is_err());
    }

    // ── candidate_count tests ────────────────────────────────────────────

    #[test]
    fn test_candidate_count() {
        let chain = make_chain(vec![
            (
                "a",
                Box::new(AlwaysFailClient {
                    fail_msg: "fail".into(),
                }),
            ),
            (
                "b",
                Box::new(AlwaysFailClient {
                    fail_msg: "fail".into(),
                }),
            ),
            (
                "c",
                Box::new(AlwaysFailClient {
                    fail_msg: "fail".into(),
                }),
            ),
        ]);
        assert_eq!(chain.candidate_count(), 3);
    }

    // ── FailoverReason Display tests ─────────────────────────────────────

    #[test]
    fn test_failover_reason_display() {
        assert_eq!(format!("{}", FailoverReason::RateLimit), "rate_limit");
        assert_eq!(format!("{}", FailoverReason::Overloaded), "overloaded");
        assert_eq!(format!("{}", FailoverReason::Timeout), "timeout");
        assert_eq!(format!("{}", FailoverReason::Auth), "auth");
        assert_eq!(format!("{}", FailoverReason::Unknown), "unknown");
    }

    // ── Three-model chain test ───────────────────────────────────────────

    #[tokio::test]
    async fn test_three_model_chain() {
        let chain = make_chain(vec![
            (
                "primary",
                Box::new(AlwaysFailClient {
                    fail_msg: "API error 429: rate limited".into(),
                }),
            ),
            (
                "secondary",
                Box::new(AlwaysFailClient {
                    fail_msg: "API error 503: overloaded".into(),
                }),
            ),
            (
                "tertiary",
                Box::new(FailThenSucceedClient::new(0, "", "tertiary")),
            ),
        ]);

        let msgs = vec![Message::user("hello")];
        let resp = chain.chat(&msgs, &[], "primary", 0.7).await.unwrap();
        let text = resp.choices[0].message.content.as_deref().unwrap();
        assert!(text.contains("tertiary"));
        // Both primary and secondary should be cooled down
        assert!(chain.is_cooled_down("primary"));
        assert!(chain.is_cooled_down("secondary"));
        assert!(!chain.is_cooled_down("tertiary"));
    }

    // ── Cooled-down candidate is skipped ──────────────────────────────────

    #[tokio::test]
    async fn test_cooled_down_candidate_skipped() {
        let chain = make_chain(vec![
            (
                "primary",
                Box::new(FailThenSucceedClient::new(0, "", "primary")),
            ),
            (
                "fallback",
                Box::new(FailThenSucceedClient::new(0, "", "fallback")),
            ),
        ]);

        // Put primary on cooldown
        chain.record_failure("primary", FailoverReason::RateLimit);

        let msgs = vec![Message::user("hello")];
        let resp = chain.chat(&msgs, &[], "primary", 0.7).await.unwrap();
        let text = resp.choices[0].message.content.as_deref().unwrap();
        // Should skip primary and use fallback
        assert!(text.contains("fallback"));
    }
}
