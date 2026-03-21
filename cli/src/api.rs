use crate::types::{ChatRequest, ChatResponse, Choice, Message, ToolDefinition, Usage};
use anyhow::{Context, Result, bail};
use std::time::Duration;

const MAX_RETRIES: u32 = 3;
const RETRY_INITIAL_DELAY_MS: u64 = 2000;
const RETRY_BACKOFF_FACTOR: u64 = 2;

/// Trait for chat completion clients — enables mock injection for testing
#[async_trait::async_trait]
pub trait ChatClient: Send + Sync {
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        temperature: f64,
    ) -> Result<ChatResponse>;
}

pub struct GrokClient {
    client: reqwest::Client,
    api_key: String,
    api_url: String,
}

impl GrokClient {
    pub fn new(api_key: String) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .context("Failed to build HTTP client")?;
        Ok(Self {
            client,
            api_key,
            api_url: "https://api.x.ai/v1/chat/completions".into(),
        })
    }
}

#[async_trait::async_trait]
impl ChatClient for GrokClient {
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        temperature: f64,
    ) -> Result<ChatResponse> {
        let request = ChatRequest {
            model: model.into(),
            messages: messages.to_vec(),
            stream: false,
            temperature,
            tools: if tools.is_empty() {
                None
            } else {
                Some(tools.to_vec())
            },
        };

        let mut last_error = String::new();

        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                let delay = RETRY_INITIAL_DELAY_MS * RETRY_BACKOFF_FACTOR.pow(attempt - 1);
                eprintln!(
                    "  Retrying in {}s (attempt {}/{})...",
                    delay / 1000,
                    attempt + 1,
                    MAX_RETRIES + 1
                );
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }

            let response = match self
                .client
                .post(&self.api_url)
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {}", self.api_key))
                .json(&request)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    last_error = format!("Network error: {e}");
                    if e.is_timeout() || e.is_connect() {
                        continue; // Retry on network errors
                    }
                    bail!("{last_error}");
                }
            };

            let status = response.status();

            // Check for Retry-After header
            let _retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());

            let body = response.text().await?;

            if status.is_success() {
                let chat_response: ChatResponse = serde_json::from_str(&body)
                    .with_context(|| format!("Failed to parse response: {body}"))?;
                return Ok(chat_response);
            }

            // Retry on 429 (rate limit) and 5xx (server errors)
            if status.as_u16() == 429 || status.is_server_error() {
                last_error = format!("API error {status}: {body}");
                continue;
            }

            // Non-retryable error
            bail!("API error {status}: {body}");
        }

        bail!("Max retries exceeded. Last error: {last_error}");
    }
}

// --- Mock client for testing ---

/// A mock chat client that returns pre-scripted responses.
/// Each call to `chat()` pops the next response from the queue.
/// Like opencode's `Bun.serve()` mock server pattern, but without HTTP.
#[cfg(test)]
pub struct MockClient {
    responses: std::sync::Mutex<Vec<Result<ChatResponse>>>,
    /// Captured requests for assertions
    pub captured: std::sync::Mutex<Vec<CapturedRequest>>,
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub struct CapturedRequest {
    pub messages: Vec<Message>,
    pub model: String,
    pub temperature: f64,
}

#[cfg(test)]
impl MockClient {
    /// Create a mock client with a queue of responses (first in, first out)
    pub fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: std::sync::Mutex::new(
                responses.into_iter().map(Ok).rev().collect(),
            ),
            captured: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Create a mock that returns an error
    pub fn with_error(msg: &str) -> Self {
        Self {
            responses: std::sync::Mutex::new(vec![Err(anyhow::anyhow!("{}", msg))]),
            captured: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Helper: build a simple text response
    pub fn text_response(text: &str) -> ChatResponse {
        ChatResponse {
            choices: vec![Choice {
                message: Message::assistant_text(text),
                finish_reason: Some("stop".into()),
            }],
            usage: Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        }
    }

    /// Helper: build a response with tool calls
    pub fn tool_call_response(calls: Vec<(String, String, String)>) -> ChatResponse {
        let tool_calls = calls
            .into_iter()
            .map(|(id, name, args)| crate::types::ToolCall {
                id,
                call_type: "function".into(),
                function: crate::types::FunctionCall {
                    name,
                    arguments: args,
                },
            })
            .collect();

        ChatResponse {
            choices: vec![Choice {
                message: Message::assistant_tool_calls(tool_calls),
                finish_reason: Some("tool_calls".into()),
            }],
            usage: Some(Usage {
                prompt_tokens: 20,
                completion_tokens: 10,
                total_tokens: 30,
            }),
        }
    }

    /// Get captured requests
    pub fn requests(&self) -> Vec<CapturedRequest> {
        self.captured.lock().unwrap().clone()
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl ChatClient for MockClient {
    async fn chat(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        model: &str,
        temperature: f64,
    ) -> Result<ChatResponse> {
        // Capture the request
        self.captured.lock().unwrap().push(CapturedRequest {
            messages: messages.to_vec(),
            model: model.into(),
            temperature,
        });

        // Pop next scripted response
        let mut queue = self.responses.lock().unwrap();
        if queue.is_empty() {
            bail!("MockClient: no more responses in queue");
        }
        queue.pop().unwrap()
    }
}
