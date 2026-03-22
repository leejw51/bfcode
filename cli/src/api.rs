use crate::types::*;
use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use std::time::Duration;

const MAX_RETRIES: u32 = 3;
const RETRY_INITIAL_DELAY_MS: u64 = 2000;
const RETRY_BACKOFF_FACTOR: u64 = 2;

/// Trait for chat completion clients — enables mock injection for testing
#[async_trait::async_trait]
pub trait ChatClient: Send + Sync {
    /// Non-streaming chat (returns full response)
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        temperature: f64,
    ) -> Result<ChatResponse>;

    /// Streaming chat — sends chunks via channel, returns final accumulated response
    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        temperature: f64,
        tx: tokio::sync::mpsc::UnboundedSender<StreamChunk>,
    ) -> Result<ChatResponse>;
}

// =============================================================================
// OpenAI-Compatible Client (works for Grok + OpenAI)
// =============================================================================

pub struct OpenAICompatibleClient {
    client: reqwest::Client,
    api_key: String,
    api_url: String,
}

/// Backward-compatible alias
pub type GrokClient = OpenAICompatibleClient;

impl OpenAICompatibleClient {
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

    pub fn from_config(config: &ProviderConfig) -> Result<Self> {
        let api_key = match std::env::var(&config.api_key_env) {
            Ok(key) => key,
            Err(_) if config.provider == Provider::Compatible => {
                // Compatible providers (Ollama, etc.) may not need an API key
                "no-key".to_string()
            }
            Err(_) => {
                anyhow::bail!("{} environment variable not set", config.api_key_env);
            }
        };
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .context("Failed to build HTTP client")?;
        Ok(Self {
            client,
            api_key,
            api_url: config.api_url.clone(),
        })
    }

    fn build_request(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        temperature: f64,
        stream: bool,
    ) -> ChatRequest {
        // Strip provider prefix for compatible models (e.g., "ollama/llama3" → "llama3")
        let model_name = model.split('/').last().unwrap_or(model);
        ChatRequest {
            model: model_name.into(),
            messages: messages.to_vec(),
            stream,
            temperature,
            tools: if tools.is_empty() {
                None
            } else {
                Some(tools.to_vec())
            },
            stream_options: if stream {
                Some(crate::types::StreamOptions {
                    include_usage: true,
                })
            } else {
                None
            },
        }
    }

    /// Build request JSON, handling image content arrays for vision
    fn build_request_json(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        temperature: f64,
        stream: bool,
    ) -> serde_json::Value {
        let has_images = messages.iter().any(|m| m.has_images());
        if !has_images {
            // Fast path: standard serialization
            let req = self.build_request(messages, tools, model, temperature, stream);
            return serde_json::to_value(&req).unwrap_or_default();
        }

        // Slow path: manually build JSON with image content arrays
        let msgs: Vec<serde_json::Value> = messages.iter().map(|m| m.to_openai_json()).collect();
        let mut obj = serde_json::json!({
            "model": model,
            "messages": msgs,
            "stream": stream,
            "temperature": temperature,
        });
        if stream {
            obj["stream_options"] = serde_json::json!({"include_usage": true});
        }
        if !tools.is_empty() {
            obj["tools"] = serde_json::to_value(tools).unwrap_or_default();
        }
        obj
    }

    async fn send_request(&self, request: &ChatRequest) -> Result<reqwest::Response> {
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
                .json(request)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    last_error = format!("Network error: {e}");
                    if e.is_timeout() || e.is_connect() {
                        continue;
                    }
                    bail!("{last_error}");
                }
            };

            let status = response.status();
            if status.is_success() {
                return Ok(response);
            }

            let body = response.text().await.unwrap_or_default();
            if status.as_u16() == 429 || status.is_server_error() {
                last_error = format!("API error {status}: {body}");
                continue;
            }
            bail!("API error {status}: {body}");
        }

        bail!("Max retries exceeded. Last error: {last_error}");
    }

    /// Send request with raw JSON body (for image content arrays)
    async fn send_request_json(&self, body: &serde_json::Value) -> Result<reqwest::Response> {
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
                .json(body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    last_error = format!("Network error: {e}");
                    if e.is_timeout() || e.is_connect() {
                        continue;
                    }
                    bail!("{last_error}");
                }
            };

            let status = response.status();
            if status.is_success() {
                return Ok(response);
            }

            let resp_body = response.text().await.unwrap_or_default();
            if status.as_u16() == 429 || status.is_server_error() {
                last_error = format!("API error {status}: {resp_body}");
                continue;
            }
            bail!("API error {status}: {resp_body}");
        }

        bail!("Max retries exceeded. Last error: {last_error}");
    }
}

#[async_trait::async_trait]
impl ChatClient for OpenAICompatibleClient {
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        temperature: f64,
    ) -> Result<ChatResponse> {
        let has_images = messages.iter().any(|m| m.has_images());
        let response = if has_images {
            let json = self.build_request_json(messages, tools, model, temperature, false);
            self.send_request_json(&json).await?
        } else {
            let request = self.build_request(messages, tools, model, temperature, false);
            self.send_request(&request).await?
        };
        let body = response.text().await?;
        let chat_response: ChatResponse = serde_json::from_str(&body).with_context(|| {
            format!("Failed to parse response: {}", &body[..body.len().min(500)])
        })?;
        Ok(chat_response)
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        temperature: f64,
        tx: tokio::sync::mpsc::UnboundedSender<StreamChunk>,
    ) -> Result<ChatResponse> {
        let has_images = messages.iter().any(|m| m.has_images());
        let response = if has_images {
            let json = self.build_request_json(messages, tools, model, temperature, true);
            self.send_request_json(&json).await?
        } else {
            let request = self.build_request(messages, tools, model, temperature, true);
            self.send_request(&request).await?
        };

        let mut accumulated_text = String::new();
        let mut accumulated_tool_calls: Vec<ToolCall> = vec![];
        let mut tool_args_buffers: Vec<String> = vec![];
        let mut usage = None;

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("Stream read error")?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            // Process complete SSE lines
            while let Some(pos) = buffer.find("\n\n") {
                let line_block = buffer[..pos].to_string();
                buffer = buffer[pos + 2..].to_string();

                for line in line_block.lines() {
                    let line = line.trim();
                    if line == "data: [DONE]" {
                        continue;
                    }
                    if let Some(data) = line.strip_prefix("data: ") {
                        if let Ok(delta) = serde_json::from_str::<StreamDelta>(data) {
                            if let Some(u) = delta.usage {
                                usage = Some(u);
                            }
                            for choice in &delta.choices {
                                if let Some(text) = &choice.delta.content {
                                    accumulated_text.push_str(text);
                                    let _ = tx.send(StreamChunk::Text(text.clone()));
                                }
                                if let Some(tcs) = &choice.delta.tool_calls {
                                    for tc_delta in tcs {
                                        let idx = tc_delta.index.unwrap_or(0);
                                        // Grow buffers as needed
                                        while accumulated_tool_calls.len() <= idx {
                                            accumulated_tool_calls.push(ToolCall {
                                                id: String::new(),
                                                call_type: "function".into(),
                                                function: FunctionCall {
                                                    name: String::new(),
                                                    arguments: String::new(),
                                                },
                                            });
                                            tool_args_buffers.push(String::new());
                                        }
                                        if let Some(id) = &tc_delta.id {
                                            accumulated_tool_calls[idx].id = id.clone();
                                        }
                                        if let Some(f) = &tc_delta.function {
                                            if let Some(name) = &f.name {
                                                accumulated_tool_calls[idx].function.name =
                                                    name.clone();
                                                let _ = tx.send(StreamChunk::ToolCallStart {
                                                    id: accumulated_tool_calls[idx].id.clone(),
                                                    name: name.clone(),
                                                });
                                            }
                                            if let Some(args) = &f.arguments {
                                                tool_args_buffers[idx].push_str(args);
                                                let _ = tx.send(StreamChunk::ToolCallDelta {
                                                    arguments: args.clone(),
                                                });
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let _ = tx.send(StreamChunk::Done);

        // Finalize tool call arguments
        for (i, tc) in accumulated_tool_calls.iter_mut().enumerate() {
            if i < tool_args_buffers.len() {
                tc.function.arguments = tool_args_buffers[i].clone();
            }
        }

        // Build the final response
        let message = if !accumulated_tool_calls.is_empty() {
            Message::assistant_tool_calls(accumulated_tool_calls)
        } else {
            Message::assistant_text(&accumulated_text)
        };

        let finish_reason = if message.tool_calls.is_some() {
            Some("tool_calls".into())
        } else {
            Some("stop".into())
        };

        Ok(ChatResponse {
            choices: vec![Choice {
                message,
                finish_reason,
            }],
            usage,
        })
    }
}

// =============================================================================
// Anthropic Client
// =============================================================================

pub struct AnthropicClient {
    client: reqwest::Client,
    api_key: String,
    api_url: String,
}

impl AnthropicClient {
    pub fn from_config(config: &ProviderConfig) -> Result<Self> {
        let api_key = std::env::var(&config.api_key_env)
            .with_context(|| format!("{} environment variable not set", config.api_key_env))?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .context("Failed to build HTTP client")?;
        Ok(Self {
            client,
            api_key,
            api_url: config.api_url.clone(),
        })
    }

    /// Convert internal messages to Anthropic format, extracting system message
    fn build_anthropic_request(
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        stream: bool,
    ) -> AnthropicRequest {
        let mut system_text = None;
        let mut anthropic_messages = Vec::new();

        for msg in messages {
            match msg.role.as_str() {
                "system" => {
                    system_text = msg.content.clone();
                }
                "user" => {
                    anthropic_messages.push(AnthropicMessage {
                        role: "user".into(),
                        content: msg.to_anthropic_content(),
                    });
                }
                "assistant" => {
                    if let Some(tool_calls) = &msg.tool_calls {
                        let blocks: Vec<serde_json::Value> = tool_calls
                            .iter()
                            .map(|tc| {
                                let input: serde_json::Value =
                                    serde_json::from_str(&tc.function.arguments)
                                        .unwrap_or(serde_json::json!({}));
                                serde_json::json!({
                                    "type": "tool_use",
                                    "id": tc.id,
                                    "name": tc.function.name,
                                    "input": input,
                                })
                            })
                            .collect();
                        // Prepend text if present
                        let mut content_blocks = Vec::new();
                        if let Some(text) = &msg.content {
                            if !text.is_empty() {
                                content_blocks
                                    .push(serde_json::json!({"type": "text", "text": text}));
                            }
                        }
                        content_blocks.extend(blocks);
                        anthropic_messages.push(AnthropicMessage {
                            role: "assistant".into(),
                            content: serde_json::json!(content_blocks),
                        });
                    } else {
                        anthropic_messages.push(AnthropicMessage {
                            role: "assistant".into(),
                            content: serde_json::json!(msg.content.as_deref().unwrap_or("")),
                        });
                    }
                }
                "tool" => {
                    anthropic_messages.push(AnthropicMessage {
                        role: "user".into(),
                        content: serde_json::json!([{
                            "type": "tool_result",
                            "tool_use_id": msg.tool_call_id.as_deref().unwrap_or(""),
                            "content": msg.content.as_deref().unwrap_or(""),
                        }]),
                    });
                }
                _ => {}
            }
        }

        let anthropic_tools: Option<Vec<AnthropicToolDef>> = if tools.is_empty() {
            None
        } else {
            Some(
                tools
                    .iter()
                    .map(|t| AnthropicToolDef {
                        name: t.function.name.clone(),
                        description: t.function.description.clone(),
                        input_schema: t.function.parameters.clone(),
                    })
                    .collect(),
            )
        };

        AnthropicRequest {
            model: model.into(),
            max_tokens: 8192,
            system: system_text,
            messages: anthropic_messages,
            stream,
            tools: anthropic_tools,
        }
    }

    /// Convert Anthropic response to internal ChatResponse
    fn convert_response(resp: AnthropicResponse) -> ChatResponse {
        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        for block in &resp.content {
            match block {
                AnthropicContentBlock::Text { text } => {
                    text_parts.push(text.clone());
                }
                AnthropicContentBlock::ToolUse { id, name, input } => {
                    tool_calls.push(ToolCall {
                        id: id.clone(),
                        call_type: "function".into(),
                        function: FunctionCall {
                            name: name.clone(),
                            arguments: serde_json::to_string(input).unwrap_or_default(),
                        },
                    });
                }
            }
        }

        let message = if !tool_calls.is_empty() {
            let mut msg = Message::assistant_tool_calls(tool_calls);
            if !text_parts.is_empty() {
                msg.content = Some(text_parts.join(""));
            }
            msg
        } else {
            Message::assistant_text(&text_parts.join(""))
        };

        let usage = resp.usage.map(|u| Usage {
            prompt_tokens: u.input_tokens,
            completion_tokens: u.output_tokens,
            total_tokens: u.input_tokens + u.output_tokens,
        });

        ChatResponse {
            choices: vec![Choice {
                message,
                finish_reason: resp.stop_reason.or(Some("stop".into())),
            }],
            usage,
        }
    }

    async fn send_request(&self, body: &AnthropicRequest) -> Result<reqwest::Response> {
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
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .json(body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    last_error = format!("Network error: {e}");
                    if e.is_timeout() || e.is_connect() {
                        continue;
                    }
                    bail!("{last_error}");
                }
            };

            let status = response.status();
            if status.is_success() {
                return Ok(response);
            }

            let body = response.text().await.unwrap_or_default();
            if status.as_u16() == 429 || status.is_server_error() {
                last_error = format!("API error {status}: {body}");
                continue;
            }
            bail!("API error {status}: {body}");
        }

        bail!("Max retries exceeded. Last error: {last_error}");
    }
}

#[async_trait::async_trait]
impl ChatClient for AnthropicClient {
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        temperature: f64,
    ) -> Result<ChatResponse> {
        let mut request = Self::build_anthropic_request(messages, tools, model, false);
        // Anthropic doesn't use temperature=0, use a small value instead
        let _ = temperature; // temperature not in AnthropicRequest for simplicity
        let response = self.send_request(&request).await?;
        let body = response.text().await?;
        let anthropic_resp: AnthropicResponse = serde_json::from_str(&body).with_context(|| {
            format!(
                "Failed to parse Anthropic response: {}",
                &body[..body.len().min(500)]
            )
        })?;
        Ok(Self::convert_response(anthropic_resp))
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        temperature: f64,
        tx: tokio::sync::mpsc::UnboundedSender<StreamChunk>,
    ) -> Result<ChatResponse> {
        let request = Self::build_anthropic_request(messages, tools, model, true);
        let _ = temperature;
        let response = self.send_request(&request).await?;

        let mut accumulated_text = String::new();
        let mut accumulated_tool_calls: Vec<ToolCall> = vec![];
        let mut current_tool_input = String::new();
        let mut usage = None;

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut current_event_type = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("Stream read error")?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(pos) = buffer.find("\n\n") {
                let line_block = buffer[..pos].to_string();
                buffer = buffer[pos + 2..].to_string();

                for line in line_block.lines() {
                    let line = line.trim();
                    if let Some(event) = line.strip_prefix("event: ") {
                        current_event_type = event.to_string();
                    } else if let Some(data) = line.strip_prefix("data: ") {
                        match current_event_type.as_str() {
                            "content_block_start" => {
                                if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                                    if v["content_block"]["type"] == "tool_use" {
                                        let id = v["content_block"]["id"]
                                            .as_str()
                                            .unwrap_or("")
                                            .to_string();
                                        let name = v["content_block"]["name"]
                                            .as_str()
                                            .unwrap_or("")
                                            .to_string();
                                        accumulated_tool_calls.push(ToolCall {
                                            id: id.clone(),
                                            call_type: "function".into(),
                                            function: FunctionCall {
                                                name: name.clone(),
                                                arguments: String::new(),
                                            },
                                        });
                                        current_tool_input.clear();
                                        let _ = tx.send(StreamChunk::ToolCallStart { id, name });
                                    }
                                }
                            }
                            "content_block_delta" => {
                                if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                                    let delta_type = v["delta"]["type"].as_str().unwrap_or("");
                                    match delta_type {
                                        "text_delta" => {
                                            if let Some(text) = v["delta"]["text"].as_str() {
                                                accumulated_text.push_str(text);
                                                let _ =
                                                    tx.send(StreamChunk::Text(text.to_string()));
                                            }
                                        }
                                        "input_json_delta" => {
                                            if let Some(json) = v["delta"]["partial_json"].as_str()
                                            {
                                                current_tool_input.push_str(json);
                                                let _ = tx.send(StreamChunk::ToolCallDelta {
                                                    arguments: json.to_string(),
                                                });
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            "content_block_stop" => {
                                // Finalize tool call arguments
                                if let Some(tc) = accumulated_tool_calls.last_mut() {
                                    if tc.function.arguments.is_empty()
                                        && !current_tool_input.is_empty()
                                    {
                                        tc.function.arguments = current_tool_input.clone();
                                        current_tool_input.clear();
                                    }
                                }
                            }
                            "message_delta" => {
                                if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                                    if let Some(u) = v.get("usage") {
                                        if let (Some(inp), Some(out)) = (
                                            u["input_tokens"].as_u64(),
                                            u["output_tokens"].as_u64(),
                                        ) {
                                            usage = Some(Usage {
                                                prompt_tokens: inp,
                                                completion_tokens: out,
                                                total_tokens: inp + out,
                                            });
                                        }
                                    }
                                }
                            }
                            "message_start" => {
                                if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                                    if let Some(u) = v.get("message").and_then(|m| m.get("usage")) {
                                        if let Some(inp) = u["input_tokens"].as_u64() {
                                            usage = Some(Usage {
                                                prompt_tokens: inp,
                                                completion_tokens: 0,
                                                total_tokens: inp,
                                            });
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        let _ = tx.send(StreamChunk::Done);

        let message = if !accumulated_tool_calls.is_empty() {
            let mut msg = Message::assistant_tool_calls(accumulated_tool_calls);
            if !accumulated_text.is_empty() {
                msg.content = Some(accumulated_text);
            }
            msg
        } else {
            Message::assistant_text(&accumulated_text)
        };

        let finish_reason = if message.tool_calls.is_some() {
            Some("tool_calls".into())
        } else {
            Some("stop".into())
        };

        Ok(ChatResponse {
            choices: vec![Choice {
                message,
                finish_reason,
            }],
            usage,
        })
    }
}

// =============================================================================
// Client Factory
// =============================================================================

/// Create the appropriate client based on config
pub fn create_client(config: &GlobalConfig) -> Result<Box<dyn ChatClient>> {
    let provider = detect_provider(&config.model);
    let provider_config = get_provider_config(&provider)?;

    match provider {
        Provider::Grok | Provider::OpenAI | Provider::Compatible => Ok(Box::new(
            OpenAICompatibleClient::from_config(&provider_config)?,
        )),
        Provider::Anthropic => Ok(Box::new(AnthropicClient::from_config(&provider_config)?)),
    }
}

// =============================================================================
// Mock Client (test only)
// =============================================================================

#[cfg(test)]
pub struct MockClient {
    responses: std::sync::Mutex<Vec<Result<ChatResponse>>>,
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
    pub fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses.into_iter().map(Ok).rev().collect()),
            captured: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn with_error(msg: &str) -> Self {
        Self {
            responses: std::sync::Mutex::new(vec![Err(anyhow::anyhow!("{}", msg))]),
            captured: std::sync::Mutex::new(Vec::new()),
        }
    }

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

    pub fn tool_call_response(calls: Vec<(String, String, String)>) -> ChatResponse {
        let tool_calls = calls
            .into_iter()
            .map(|(id, name, args)| ToolCall {
                id,
                call_type: "function".into(),
                function: FunctionCall {
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
        self.captured.lock().unwrap().push(CapturedRequest {
            messages: messages.to_vec(),
            model: model.into(),
            temperature,
        });

        let mut queue = self.responses.lock().unwrap();
        if queue.is_empty() {
            bail!("MockClient: no more responses in queue");
        }
        queue.pop().unwrap()
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        temperature: f64,
        tx: tokio::sync::mpsc::UnboundedSender<StreamChunk>,
    ) -> Result<ChatResponse> {
        // For mock: just get the response and send all text at once
        let response = self.chat(messages, tools, model, temperature).await?;
        if let Some(choice) = response.choices.first() {
            if let Some(text) = &choice.message.content {
                let _ = tx.send(StreamChunk::Text(text.clone()));
            }
        }
        let _ = tx.send(StreamChunk::Done);
        Ok(response)
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_anthropic_request_conversion() {
        let messages = vec![
            Message::system("You are helpful."),
            Message::user("Hello"),
            Message::assistant_text("Hi there!"),
            Message::user("Read a file"),
        ];
        let tools = vec![];

        let req = AnthropicClient::build_anthropic_request(
            &messages,
            &tools,
            "claude-sonnet-4-20250514",
            false,
        );

        assert_eq!(req.model, "claude-sonnet-4-20250514");
        assert_eq!(req.system.as_deref(), Some("You are helpful."));
        assert_eq!(req.messages.len(), 3); // system extracted, 3 remaining
        assert_eq!(req.messages[0].role, "user");
        assert_eq!(req.messages[1].role, "assistant");
        assert_eq!(req.messages[2].role, "user");
        assert!(!req.stream);
    }

    #[test]
    fn test_anthropic_request_with_tool_calls() {
        let tc = ToolCall {
            id: "tu_1".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "read".into(),
                arguments: r#"{"path":"foo.rs"}"#.into(),
            },
        };
        let messages = vec![
            Message::user("Read foo.rs"),
            Message::assistant_tool_calls(vec![tc]),
            Message::tool_result("tu_1", "fn main() {}"),
        ];
        let tools = vec![];

        let req = AnthropicClient::build_anthropic_request(
            &messages,
            &tools,
            "claude-sonnet-4-20250514",
            false,
        );

        // tool_result becomes user message in Anthropic format
        assert_eq!(req.messages.len(), 3);
        assert_eq!(req.messages[2].role, "user");
        // Check tool_result content block
        let content = &req.messages[2].content;
        assert!(content.is_array());
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "tu_1");
    }

    #[test]
    fn test_anthropic_response_conversion() {
        let resp = AnthropicResponse {
            content: vec![AnthropicContentBlock::Text {
                text: "Hello!".into(),
            }],
            usage: Some(AnthropicUsage {
                input_tokens: 10,
                output_tokens: 5,
            }),
            stop_reason: Some("end_turn".into()),
        };

        let chat_resp = AnthropicClient::convert_response(resp);
        assert_eq!(chat_resp.choices.len(), 1);
        assert_eq!(
            chat_resp.choices[0].message.content.as_deref(),
            Some("Hello!")
        );
        assert_eq!(chat_resp.usage.as_ref().unwrap().total_tokens, 15);
    }

    #[test]
    fn test_anthropic_response_with_tool_use() {
        let resp = AnthropicResponse {
            content: vec![
                AnthropicContentBlock::Text {
                    text: "Let me read that.".into(),
                },
                AnthropicContentBlock::ToolUse {
                    id: "tu_1".into(),
                    name: "read".into(),
                    input: serde_json::json!({"path": "foo.rs"}),
                },
            ],
            usage: Some(AnthropicUsage {
                input_tokens: 20,
                output_tokens: 10,
            }),
            stop_reason: Some("tool_use".into()),
        };

        let chat_resp = AnthropicClient::convert_response(resp);
        let msg = &chat_resp.choices[0].message;
        assert!(msg.tool_calls.is_some());
        let tcs = msg.tool_calls.as_ref().unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].function.name, "read");
        // Text should also be preserved
        assert_eq!(msg.content.as_deref(), Some("Let me read that."));
    }

    #[test]
    fn test_anthropic_tools_conversion() {
        let tools = vec![ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "read".into(),
                description: "Read a file".into(),
                parameters: serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            },
        }];

        let req = AnthropicClient::build_anthropic_request(
            &[Message::user("hi")],
            &tools,
            "claude-sonnet-4-20250514",
            false,
        );

        let at = req.tools.unwrap();
        assert_eq!(at.len(), 1);
        assert_eq!(at[0].name, "read");
        assert_eq!(at[0].description, "Read a file");
    }

    #[test]
    fn test_create_client_factory_detects_provider() {
        // Can't actually create clients without API keys, but we can test detection
        let provider = detect_provider("gpt-4o");
        assert_eq!(provider, Provider::OpenAI);

        let provider = detect_provider("claude-sonnet-4-20250514");
        assert_eq!(provider, Provider::Anthropic);

        let provider = detect_provider("grok-4-1-fast");
        assert_eq!(provider, Provider::Grok);
    }

    #[tokio::test]
    async fn test_mock_client_stream() {
        let mock = MockClient::new(vec![MockClient::text_response("streamed text")]);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let response = mock
            .chat_stream(&[Message::user("hi")], &[], "test", 0.0, tx)
            .await
            .unwrap();

        // Should have received text + done
        let mut received = Vec::new();
        while let Ok(chunk) = rx.try_recv() {
            received.push(chunk);
        }

        assert!(received.len() >= 2); // Text + Done
        assert!(matches!(&received[0], StreamChunk::Text(t) if t == "streamed text"));
        assert!(matches!(&received[1], StreamChunk::Done));
        assert_eq!(
            response.choices[0].message.content.as_deref(),
            Some("streamed text")
        );
    }

    // ── Streaming with tool calls ────────────────────────────────────

    #[tokio::test]
    async fn test_mock_client_stream_tool_calls() {
        let mock = MockClient::new(vec![MockClient::tool_call_response(vec![(
            "c1".into(),
            "read".into(),
            r#"{"path":"foo.rs"}"#.into(),
        )])]);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let response = mock
            .chat_stream(&[Message::user("read foo")], &[], "test", 0.0, tx)
            .await
            .unwrap();

        // Should get Done (no text chunks for tool call responses)
        let mut received = Vec::new();
        while let Ok(chunk) = rx.try_recv() {
            received.push(chunk);
        }
        assert!(received.iter().any(|c| matches!(c, StreamChunk::Done)));

        // Response should have tool calls
        assert!(response.choices[0].message.tool_calls.is_some());
    }

    #[tokio::test]
    async fn test_mock_client_stream_error() {
        let mock = MockClient::with_error("server down");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let result = mock
            .chat_stream(&[Message::user("hi")], &[], "test", 0.0, tx)
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("server down"));
    }

    #[tokio::test]
    async fn test_mock_client_stream_captures_requests() {
        let mock = MockClient::new(vec![MockClient::text_response("ok")]);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        mock.chat_stream(
            &[Message::system("sys"), Message::user("hello")],
            &[],
            "test-model",
            0.5,
            tx,
        )
        .await
        .unwrap();

        let reqs = mock.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].model, "test-model");
        assert_eq!(reqs[0].temperature, 0.5);
        assert_eq!(reqs[0].messages.len(), 2);
    }

    // ── Anthropic format edge cases ──────────────────────────────────

    #[test]
    fn test_anthropic_request_no_system_message() {
        let messages = vec![Message::user("Hello")];
        let req = AnthropicClient::build_anthropic_request(
            &messages,
            &[],
            "claude-sonnet-4-20250514",
            false,
        );
        assert!(req.system.is_none());
        assert_eq!(req.messages.len(), 1);
    }

    #[test]
    fn test_anthropic_request_multiple_system_messages() {
        // Only the last system message should be used
        let messages = vec![
            Message::system("First system"),
            Message::system("Second system"),
            Message::user("Hello"),
        ];
        let req = AnthropicClient::build_anthropic_request(
            &messages,
            &[],
            "claude-sonnet-4-20250514",
            false,
        );
        // Second system overwrites first
        assert_eq!(req.system.as_deref(), Some("Second system"));
        assert_eq!(req.messages.len(), 1);
    }

    #[test]
    fn test_anthropic_request_stream_flag() {
        let messages = vec![Message::user("Hello")];
        let req_stream = AnthropicClient::build_anthropic_request(
            &messages,
            &[],
            "claude-sonnet-4-20250514",
            true,
        );
        let req_no_stream = AnthropicClient::build_anthropic_request(
            &messages,
            &[],
            "claude-sonnet-4-20250514",
            false,
        );
        assert!(req_stream.stream);
        assert!(!req_no_stream.stream);
    }

    #[test]
    fn test_anthropic_response_multiple_text_blocks() {
        let resp = AnthropicResponse {
            content: vec![
                AnthropicContentBlock::Text {
                    text: "Hello ".into(),
                },
                AnthropicContentBlock::Text {
                    text: "World!".into(),
                },
            ],
            usage: None,
            stop_reason: Some("end_turn".into()),
        };
        let chat_resp = AnthropicClient::convert_response(resp);
        assert_eq!(
            chat_resp.choices[0].message.content.as_deref(),
            Some("Hello World!")
        );
    }

    #[test]
    fn test_anthropic_response_no_usage() {
        let resp = AnthropicResponse {
            content: vec![AnthropicContentBlock::Text { text: "Hi".into() }],
            usage: None,
            stop_reason: None,
        };
        let chat_resp = AnthropicClient::convert_response(resp);
        assert!(chat_resp.usage.is_none());
    }

    #[test]
    fn test_anthropic_response_multiple_tool_uses() {
        let resp = AnthropicResponse {
            content: vec![
                AnthropicContentBlock::ToolUse {
                    id: "tu_1".into(),
                    name: "read".into(),
                    input: serde_json::json!({"path": "a.rs"}),
                },
                AnthropicContentBlock::ToolUse {
                    id: "tu_2".into(),
                    name: "read".into(),
                    input: serde_json::json!({"path": "b.rs"}),
                },
            ],
            usage: Some(AnthropicUsage {
                input_tokens: 10,
                output_tokens: 20,
            }),
            stop_reason: Some("tool_use".into()),
        };
        let chat_resp = AnthropicClient::convert_response(resp);
        let tcs = chat_resp.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(tcs.len(), 2);
        assert_eq!(tcs[0].id, "tu_1");
        assert_eq!(tcs[1].id, "tu_2");
    }

    // ── Provider config ──────────────────────────────────────────────

    #[test]
    fn test_get_provider_config() {
        let grok = get_provider_config(&Provider::Grok).unwrap();
        assert_eq!(grok.api_key_env, "GROK_API_KEY");
        assert!(grok.api_url.contains("x.ai"));

        let openai = get_provider_config(&Provider::OpenAI).unwrap();
        assert_eq!(openai.api_key_env, "OPENAI_API_KEY");
        assert!(openai.api_url.contains("openai.com"));

        let anthropic = get_provider_config(&Provider::Anthropic).unwrap();
        assert_eq!(anthropic.api_key_env, "ANTHROPIC_API_KEY");
        assert!(anthropic.api_url.contains("anthropic.com"));
    }

    // ── OpenAI-compatible request building ───────────────────────────

    #[test]
    fn test_openai_compatible_build_request_no_tools() {
        let client = OpenAICompatibleClient {
            client: reqwest::Client::new(),
            api_key: "test".into(),
            api_url: "http://localhost".into(),
        };
        let req = client.build_request(&[Message::user("hi")], &[], "gpt-4o", 0.7, true);
        assert_eq!(req.model, "gpt-4o");
        assert!(req.stream);
        assert_eq!(req.temperature, 0.7);
        assert!(req.tools.is_none());
    }

    #[test]
    fn test_openai_compatible_build_request_with_tools() {
        let client = OpenAICompatibleClient {
            client: reqwest::Client::new(),
            api_key: "test".into(),
            api_url: "http://localhost".into(),
        };
        let tools = vec![ToolDefinition {
            tool_type: "function".into(),
            function: FunctionSchema {
                name: "read".into(),
                description: "Read".into(),
                parameters: serde_json::json!({}),
            },
        }];
        let req = client.build_request(&[Message::user("hi")], &tools, "gpt-4o", 0.0, false);
        assert!(req.tools.is_some());
        assert_eq!(req.tools.unwrap().len(), 1);
        assert!(!req.stream);
    }
}
