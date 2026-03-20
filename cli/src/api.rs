use crate::types::{ChatRequest, ChatResponse, Message, ToolDefinition};
use std::time::Duration;

const MAX_RETRIES: u32 = 3;
const RETRY_INITIAL_DELAY_MS: u64 = 2000;
const RETRY_BACKOFF_FACTOR: u64 = 2;

pub struct GrokClient {
    client: reqwest::Client,
    api_key: String,
    api_url: String,
}

impl GrokClient {
    pub fn new(api_key: String) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            api_key,
            api_url: "https://api.x.ai/v1/chat/completions".into(),
        }
    }

    pub async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        temperature: f64,
    ) -> Result<ChatResponse, Box<dyn std::error::Error>> {
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
                    return Err(last_error.into());
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
                    .map_err(|e| format!("Failed to parse response: {e}\nBody: {body}"))?;
                return Ok(chat_response);
            }

            // Retry on 429 (rate limit) and 5xx (server errors)
            if status.as_u16() == 429 || status.is_server_error() {
                last_error = format!("API error {status}: {body}");
                continue;
            }

            // Non-retryable error
            return Err(format!("API error {status}: {body}").into());
        }

        Err(format!("Max retries exceeded. Last error: {last_error}").into())
    }
}
