//! MCP (Model Context Protocol) client implementation.
//!
//! Supports local (stdio) and remote (HTTP/SSE) MCP servers.
//! Discovers tools via `tools/list` and executes them via `tools/call`.

use crate::types::{FunctionSchema, ToolDefinition};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

// ---------------------------------------------------------------------------
// Configuration types
// ---------------------------------------------------------------------------

/// MCP server configuration (stored in config.json under "mcp_servers").
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum McpServerConfig {
    /// Local server — launched as a child process, communicates over stdio.
    #[serde(rename = "local")]
    Local {
        /// Command and arguments (e.g. ["npx", "-y", "@modelcontextprotocol/server-filesystem", "/tmp"])
        command: Vec<String>,
        /// Extra environment variables for the child process.
        #[serde(default)]
        environment: HashMap<String, String>,
        /// Whether the server is enabled (default true).
        #[serde(default = "default_true")]
        enabled: bool,
        /// Request timeout in milliseconds (default 30000).
        #[serde(default = "default_timeout")]
        timeout: u64,
    },
    /// Remote server — communicates over HTTP/SSE.
    #[serde(rename = "remote")]
    Remote {
        /// URL of the remote MCP server.
        url: String,
        /// Custom HTTP headers.
        #[serde(default)]
        headers: HashMap<String, String>,
        /// Whether the server is enabled (default true).
        #[serde(default = "default_true")]
        enabled: bool,
        /// Request timeout in milliseconds (default 30000).
        #[serde(default = "default_timeout")]
        timeout: u64,
    },
}

fn default_true() -> bool {
    true
}
fn default_timeout() -> u64 {
    30_000
}

impl McpServerConfig {
    pub fn is_enabled(&self) -> bool {
        match self {
            McpServerConfig::Local { enabled, .. } => *enabled,
            McpServerConfig::Remote { enabled, .. } => *enabled,
        }
    }

    pub fn timeout_ms(&self) -> u64 {
        match self {
            McpServerConfig::Local { timeout, .. } => *timeout,
            McpServerConfig::Remote { timeout, .. } => *timeout,
        }
    }
}

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    id: serde_json::Value,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
}

#[derive(Deserialize, Debug)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: Option<String>,
    #[allow(dead_code)]
    id: Option<serde_json::Value>,
    result: Option<serde_json::Value>,
    error: Option<JsonRpcError>,
}

#[derive(Deserialize, Debug)]
struct JsonRpcError {
    code: i64,
    message: String,
    #[allow(dead_code)]
    data: Option<serde_json::Value>,
}

impl std::fmt::Display for JsonRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MCP error {}: {}", self.code, self.message)
    }
}

// ---------------------------------------------------------------------------
// MCP protocol types
// ---------------------------------------------------------------------------

#[derive(Deserialize, Debug, Clone)]
pub struct McpToolInfo {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Option<serde_json::Value>,
}

#[derive(Deserialize, Debug)]
struct ToolsListResult {
    tools: Vec<McpToolInfo>,
}

#[derive(Deserialize, Debug)]
struct CallToolResult {
    content: Vec<ToolContent>,
    #[serde(default)]
    #[allow(dead_code)]
    is_error: Option<bool>,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
enum ToolContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        #[allow(dead_code)]
        mime_type: String,
    },
    #[serde(rename = "resource")]
    Resource { resource: ResourceContent },
}

#[derive(Deserialize, Debug)]
struct ResourceContent {
    #[allow(dead_code)]
    uri: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

// ---------------------------------------------------------------------------
// Transport trait
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
trait McpTransport: Send + Sync {
    async fn send_request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout_ms: u64,
    ) -> Result<serde_json::Value>;

    async fn shutdown(&self) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Stdio transport
// ---------------------------------------------------------------------------

struct StdioTransport {
    child: Arc<Mutex<Child>>,
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    stdout: Arc<Mutex<BufReader<tokio::process::ChildStdout>>>,
    next_id: Arc<Mutex<u64>>,
}

impl StdioTransport {
    async fn new(command: &[String], environment: &HashMap<String, String>) -> Result<Self> {
        if command.is_empty() {
            bail!("MCP local server command is empty");
        }

        let mut cmd = Command::new(&command[0]);
        if command.len() > 1 {
            cmd.args(&command[1..]);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        for (k, v) in environment {
            cmd.env(k, v);
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawning MCP server: {}", command.join(" ")))?;

        let stdin = child
            .stdin
            .take()
            .context("failed to capture MCP server stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("failed to capture MCP server stdout")?;

        // Spawn a task to drain stderr so it doesn't block
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    eprintln!("[mcp stderr] {line}");
                }
            });
        }

        Ok(Self {
            child: Arc::new(Mutex::new(child)),
            stdin: Arc::new(Mutex::new(stdin)),
            stdout: Arc::new(Mutex::new(BufReader::new(stdout))),
            next_id: Arc::new(Mutex::new(1)),
        })
    }
}

#[async_trait::async_trait]
impl McpTransport for StdioTransport {
    async fn send_request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout_ms: u64,
    ) -> Result<serde_json::Value> {
        let id = {
            let mut next = self.next_id.lock().await;
            let id = *next;
            *next += 1;
            id
        };

        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id: serde_json::Value::Number(id.into()),
            method: method.to_string(),
            params,
        };

        let mut payload = serde_json::to_string(&request)?;
        payload.push('\n');

        // Write request
        {
            let mut stdin = self.stdin.lock().await;
            stdin
                .write_all(payload.as_bytes())
                .await
                .context("writing to MCP server stdin")?;
            stdin.flush().await?;
        }

        // Read response (with timeout)
        let response_line =
            tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), async {
                let mut stdout = self.stdout.lock().await;
                let mut line = String::new();
                loop {
                    line.clear();
                    let bytes_read = stdout
                        .read_line(&mut line)
                        .await
                        .context("reading from MCP server stdout")?;
                    if bytes_read == 0 {
                        bail!("MCP server closed stdout unexpectedly");
                    }
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    // Skip JSON-RPC notifications (no id field)
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
                        if v.get("id").is_some() {
                            return Ok(line.clone());
                        }
                        // else it's a notification, skip it
                        continue;
                    }
                    continue;
                }
            })
            .await
            .context("MCP request timed out")?
            .context("reading MCP response")?;

        let resp: JsonRpcResponse =
            serde_json::from_str(response_line.trim()).context("parsing MCP JSON-RPC response")?;

        if let Some(err) = resp.error {
            bail!("{err}");
        }

        resp.result
            .ok_or_else(|| anyhow::anyhow!("MCP response missing both result and error"))
    }

    async fn shutdown(&self) -> Result<()> {
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// HTTP transport (for remote MCP servers)
// ---------------------------------------------------------------------------

struct HttpTransport {
    url: String,
    headers: HashMap<String, String>,
    client: reqwest::Client,
    next_id: Arc<Mutex<u64>>,
}

impl HttpTransport {
    fn new(url: &str, headers: &HashMap<String, String>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?;
        Ok(Self {
            url: url.to_string(),
            headers: headers.clone(),
            client,
            next_id: Arc::new(Mutex::new(1)),
        })
    }
}

#[async_trait::async_trait]
impl McpTransport for HttpTransport {
    async fn send_request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout_ms: u64,
    ) -> Result<serde_json::Value> {
        let id = {
            let mut next = self.next_id.lock().await;
            let id = *next;
            *next += 1;
            id
        };

        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id: serde_json::Value::Number(id.into()),
            method: method.to_string(),
            params,
        };

        let mut req_builder = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_millis(timeout_ms));

        for (k, v) in &self.headers {
            req_builder = req_builder.header(k, v);
        }

        let response = req_builder
            .json(&request)
            .send()
            .await
            .context("sending MCP HTTP request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_else(|_| "<no body>".into());
            bail!("MCP HTTP error {status}: {body}");
        }

        let resp: JsonRpcResponse = response
            .json()
            .await
            .context("parsing MCP HTTP JSON-RPC response")?;

        if let Some(err) = resp.error {
            bail!("{err}");
        }

        resp.result
            .ok_or_else(|| anyhow::anyhow!("MCP response missing both result and error"))
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// MCP Client (per-server)
// ---------------------------------------------------------------------------

pub struct McpClient {
    name: String,
    transport: Box<dyn McpTransport>,
    tools: Vec<McpToolInfo>,
    timeout_ms: u64,
}

impl McpClient {
    /// Connect to a local MCP server (stdio transport).
    async fn connect_local(
        name: &str,
        command: &[String],
        environment: &HashMap<String, String>,
        timeout_ms: u64,
    ) -> Result<Self> {
        let transport = StdioTransport::new(command, environment).await?;
        let mut client = Self {
            name: name.to_string(),
            transport: Box::new(transport),
            tools: Vec::new(),
            timeout_ms,
        };
        client.initialize().await?;
        client.discover_tools().await?;
        Ok(client)
    }

    /// Connect to a remote MCP server (HTTP transport).
    async fn connect_remote(
        name: &str,
        url: &str,
        headers: &HashMap<String, String>,
        timeout_ms: u64,
    ) -> Result<Self> {
        let transport = HttpTransport::new(url, headers)?;
        let mut client = Self {
            name: name.to_string(),
            transport: Box::new(transport),
            tools: Vec::new(),
            timeout_ms,
        };
        client.initialize().await?;
        client.discover_tools().await?;
        Ok(client)
    }

    /// Send the MCP `initialize` handshake.
    async fn initialize(&self) -> Result<()> {
        let params = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "bfcode",
                "version": "0.6.0"
            }
        });

        self.transport
            .send_request("initialize", Some(params), self.timeout_ms)
            .await
            .context("MCP initialize handshake failed")?;

        // Send initialized notification (no id, no response expected)
        // For stdio, we send it but don't wait for a response.
        // We use a small trick: send as request but ignore errors since
        // notifications don't get responses. Instead we just send the raw
        // notification via the transport but with a dummy id and ignore timeout.
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            self.transport
                .send_request("notifications/initialized", None, 500),
        )
        .await;

        Ok(())
    }

    /// Discover available tools from the server.
    async fn discover_tools(&mut self) -> Result<()> {
        let result = self
            .transport
            .send_request("tools/list", Some(serde_json::json!({})), self.timeout_ms)
            .await
            .context("MCP tools/list failed")?;

        let list: ToolsListResult =
            serde_json::from_value(result).context("parsing MCP tools/list result")?;

        self.tools = list.tools;
        Ok(())
    }

    /// Call a tool on this server.
    pub async fn call_tool(&self, tool_name: &str, arguments: serde_json::Value) -> Result<String> {
        let params = serde_json::json!({
            "name": tool_name,
            "arguments": arguments,
        });

        let result = self
            .transport
            .send_request("tools/call", Some(params), self.timeout_ms)
            .await
            .with_context(|| format!("MCP tools/call '{tool_name}' failed"))?;

        let call_result: CallToolResult =
            serde_json::from_value(result).context("parsing MCP tools/call result")?;

        // Concatenate all text content
        let mut output = String::new();
        for content in &call_result.content {
            match content {
                ToolContent::Text { text } => {
                    if !output.is_empty() {
                        output.push('\n');
                    }
                    output.push_str(text);
                }
                ToolContent::Image { data, .. } => {
                    if !output.is_empty() {
                        output.push('\n');
                    }
                    output.push_str(&format!("[image: {} bytes base64]", data.len()));
                }
                ToolContent::Resource { resource } => {
                    if let Some(text) = &resource.text {
                        if !output.is_empty() {
                            output.push('\n');
                        }
                        output.push_str(text);
                    }
                }
            }
        }

        Ok(output)
    }

    /// Get the list of discovered tools.
    pub fn tools(&self) -> &[McpToolInfo] {
        &self.tools
    }

    /// Server name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Shutdown the server connection.
    pub async fn shutdown(&self) -> Result<()> {
        self.transport.shutdown().await
    }
}

// ---------------------------------------------------------------------------
// MCP Manager — manages all connected servers
// ---------------------------------------------------------------------------

pub struct McpManager {
    clients: Vec<McpClient>,
}

impl McpManager {
    /// Create a new empty manager.
    pub fn new() -> Self {
        Self {
            clients: Vec::new(),
        }
    }

    /// Connect to all enabled MCP servers from config.
    pub async fn connect_all(servers: &HashMap<String, McpServerConfig>) -> Self {
        let mut manager = Self::new();

        for (name, config) in servers {
            if !config.is_enabled() {
                eprintln!(
                    "  {} MCP server '{}' (disabled)",
                    "○".dimmed(),
                    name.dimmed()
                );
                continue;
            }

            match Self::connect_one(name, config).await {
                Ok(client) => {
                    let tool_count = client.tools().len();
                    eprintln!(
                        "  {} MCP server '{}' ({} tools)",
                        "✓".green(),
                        name.cyan(),
                        tool_count
                    );
                    manager.clients.push(client);
                }
                Err(e) => {
                    eprintln!("  {} MCP server '{}': {}", "✗".red(), name.yellow(), e);
                }
            }
        }

        manager
    }

    /// Connect to a single MCP server.
    async fn connect_one(name: &str, config: &McpServerConfig) -> Result<McpClient> {
        match config {
            McpServerConfig::Local {
                command,
                environment,
                timeout,
                ..
            } => McpClient::connect_local(name, command, environment, *timeout).await,
            McpServerConfig::Remote {
                url,
                headers,
                timeout,
                ..
            } => McpClient::connect_remote(name, url, headers, *timeout).await,
        }
    }

    /// Get tool definitions for all connected servers, converted to bfcode's ToolDefinition format.
    /// Tool names are prefixed with `mcp_{server_name}_` to avoid collisions.
    pub fn get_tool_definitions(&self) -> Vec<ToolDefinition> {
        let mut defs = Vec::new();
        for client in &self.clients {
            for tool in client.tools() {
                let prefixed_name = format!("mcp_{}_{}", client.name(), tool.name);
                let description = tool
                    .description
                    .clone()
                    .unwrap_or_else(|| format!("MCP tool: {}", tool.name));

                let parameters = tool
                    .input_schema
                    .clone()
                    .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));

                defs.push(ToolDefinition {
                    tool_type: "function".into(),
                    function: FunctionSchema {
                        name: prefixed_name,
                        description: format!("[MCP: {}] {}", client.name(), description),
                        parameters,
                    },
                });
            }
        }
        defs
    }

    /// Execute an MCP tool call. The `prefixed_name` should be like `mcp_servername_toolname`.
    pub async fn execute_tool(&self, prefixed_name: &str, arguments: &str) -> Result<String> {
        // Parse: mcp_{server}_{tool}
        let rest = prefixed_name
            .strip_prefix("mcp_")
            .context("MCP tool name must start with mcp_")?;

        // Find which client owns this tool by trying each
        for client in &self.clients {
            let prefix = format!("{}_", client.name());
            if let Some(tool_name) = rest.strip_prefix(&prefix) {
                // Verify this tool exists on this client
                if client.tools().iter().any(|t| t.name == tool_name) {
                    let args: serde_json::Value =
                        serde_json::from_str(arguments).unwrap_or(serde_json::json!({}));
                    return client.call_tool(tool_name, args).await;
                }
            }
        }

        bail!("No MCP server found for tool: {prefixed_name}");
    }

    /// Check if a tool name is an MCP tool.
    pub fn is_mcp_tool(&self, name: &str) -> bool {
        name.starts_with("mcp_")
    }

    /// List all connected servers and their tools.
    pub fn status_report(&self) -> String {
        if self.clients.is_empty() {
            return "No MCP servers connected.".to_string();
        }

        let mut report = String::new();
        for client in &self.clients {
            report.push_str(&format!(
                "MCP server '{}': {} tools\n",
                client.name(),
                client.tools().len()
            ));
            for tool in client.tools() {
                let desc = tool.description.as_deref().unwrap_or("(no description)");
                report.push_str(&format!("  - {}: {}\n", tool.name, desc));
            }
        }
        report
    }

    /// Shutdown all connected servers.
    pub async fn shutdown_all(&self) {
        for client in &self.clients {
            if let Err(e) = client.shutdown().await {
                eprintln!(
                    "Warning: failed to shutdown MCP server '{}': {e}",
                    client.name()
                );
            }
        }
    }
}

use colored::Colorize;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- Config deserialization tests ---

    #[test]
    fn test_parse_local_config() {
        let json = r#"{
            "type": "local",
            "command": ["npx", "-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
            "environment": {"FOO": "bar"},
            "enabled": true,
            "timeout": 5000
        }"#;
        let config: McpServerConfig = serde_json::from_str(json).unwrap();
        match &config {
            McpServerConfig::Local {
                command,
                environment,
                enabled,
                timeout,
            } => {
                assert_eq!(
                    command,
                    &[
                        "npx",
                        "-y",
                        "@modelcontextprotocol/server-filesystem",
                        "/tmp"
                    ]
                );
                assert_eq!(environment.get("FOO").unwrap(), "bar");
                assert!(*enabled);
                assert_eq!(*timeout, 5000);
            }
            _ => panic!("expected Local config"),
        }
        assert!(config.is_enabled());
        assert_eq!(config.timeout_ms(), 5000);
    }

    #[test]
    fn test_parse_remote_config() {
        let json = r#"{
            "type": "remote",
            "url": "https://mcp.example.com/rpc",
            "headers": {"Authorization": "Bearer token123"},
            "enabled": false,
            "timeout": 10000
        }"#;
        let config: McpServerConfig = serde_json::from_str(json).unwrap();
        match &config {
            McpServerConfig::Remote {
                url,
                headers,
                enabled,
                timeout,
            } => {
                assert_eq!(url, "https://mcp.example.com/rpc");
                assert_eq!(headers.get("Authorization").unwrap(), "Bearer token123");
                assert!(!*enabled);
                assert_eq!(*timeout, 10000);
            }
            _ => panic!("expected Remote config"),
        }
        assert!(!config.is_enabled());
        assert_eq!(config.timeout_ms(), 10000);
    }

    #[test]
    fn test_parse_local_config_defaults() {
        let json = r#"{
            "type": "local",
            "command": ["echo", "hello"]
        }"#;
        let config: McpServerConfig = serde_json::from_str(json).unwrap();
        assert!(config.is_enabled()); // default true
        assert_eq!(config.timeout_ms(), 30_000); // default 30s
        match &config {
            McpServerConfig::Local { environment, .. } => {
                assert!(environment.is_empty()); // default empty
            }
            _ => panic!("expected Local config"),
        }
    }

    #[test]
    fn test_parse_remote_config_defaults() {
        let json = r#"{
            "type": "remote",
            "url": "https://example.com/mcp"
        }"#;
        let config: McpServerConfig = serde_json::from_str(json).unwrap();
        assert!(config.is_enabled());
        assert_eq!(config.timeout_ms(), 30_000);
        match &config {
            McpServerConfig::Remote { headers, .. } => {
                assert!(headers.is_empty());
            }
            _ => panic!("expected Remote config"),
        }
    }

    #[test]
    fn test_parse_mcp_servers_map() {
        let json = r#"{
            "filesystem": {
                "type": "local",
                "command": ["npx", "server"],
                "enabled": true
            },
            "remote-api": {
                "type": "remote",
                "url": "https://api.example.com/mcp",
                "enabled": false
            }
        }"#;
        let servers: HashMap<String, McpServerConfig> = serde_json::from_str(json).unwrap();
        assert_eq!(servers.len(), 2);
        assert!(servers.contains_key("filesystem"));
        assert!(servers.contains_key("remote-api"));
        assert!(servers["filesystem"].is_enabled());
        assert!(!servers["remote-api"].is_enabled());
    }

    // --- JSON-RPC type tests ---

    #[test]
    fn test_jsonrpc_request_serialization() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id: serde_json::Value::Number(1.into()),
            method: "tools/list".to_string(),
            params: Some(serde_json::json!({})),
        };
        let json = serde_json::to_string(&req).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 1);
        assert_eq!(v["method"], "tools/list");
    }

    #[test]
    fn test_jsonrpc_request_no_params() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id: serde_json::Value::Number(1.into()),
            method: "notifications/initialized".to_string(),
            params: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v.get("params").is_none());
    }

    #[test]
    fn test_jsonrpc_response_with_result() {
        let json = r#"{
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"tools": []}
        }"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn test_jsonrpc_response_with_error() {
        let json = r#"{
            "jsonrpc": "2.0",
            "id": 1,
            "error": {"code": -32600, "message": "Invalid Request"}
        }"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert!(resp.result.is_none());
        let err = resp.error.unwrap();
        assert_eq!(err.code, -32600);
        assert_eq!(err.message, "Invalid Request");
        assert_eq!(format!("{err}"), "MCP error -32600: Invalid Request");
    }

    // --- MCP protocol type tests ---

    #[test]
    fn test_parse_tools_list_result() {
        let json = r#"{
            "tools": [
                {
                    "name": "read_file",
                    "description": "Read a file",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "path": {"type": "string"}
                        },
                        "required": ["path"]
                    }
                },
                {
                    "name": "list_dir",
                    "description": "List directory"
                }
            ]
        }"#;
        let result: ToolsListResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.tools.len(), 2);
        assert_eq!(result.tools[0].name, "read_file");
        assert_eq!(result.tools[0].description.as_deref(), Some("Read a file"));
        assert!(result.tools[0].input_schema.is_some());
        assert_eq!(result.tools[1].name, "list_dir");
    }

    #[test]
    fn test_parse_tools_list_empty() {
        let json = r#"{"tools": []}"#;
        let result: ToolsListResult = serde_json::from_str(json).unwrap();
        assert!(result.tools.is_empty());
    }

    #[test]
    fn test_parse_tool_content_text() {
        let json = r#"{"type": "text", "text": "hello world"}"#;
        let content: ToolContent = serde_json::from_str(json).unwrap();
        match content {
            ToolContent::Text { text } => assert_eq!(text, "hello world"),
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn test_parse_tool_content_image() {
        let json = r#"{"type": "image", "data": "aGVsbG8=", "mimeType": "image/png"}"#;
        let content: ToolContent = serde_json::from_str(json).unwrap();
        match content {
            ToolContent::Image { data, mime_type } => {
                assert_eq!(data, "aGVsbG8=");
                assert_eq!(mime_type, "image/png");
            }
            _ => panic!("expected Image"),
        }
    }

    #[test]
    fn test_parse_tool_content_resource() {
        let json = r#"{"type": "resource", "resource": {"uri": "file:///tmp/test.txt", "text": "contents"}}"#;
        let content: ToolContent = serde_json::from_str(json).unwrap();
        match content {
            ToolContent::Resource { resource } => {
                assert_eq!(resource.uri.as_deref(), Some("file:///tmp/test.txt"));
                assert_eq!(resource.text.as_deref(), Some("contents"));
            }
            _ => panic!("expected Resource"),
        }
    }

    #[test]
    fn test_parse_call_tool_result() {
        let json = r#"{
            "content": [
                {"type": "text", "text": "line 1"},
                {"type": "text", "text": "line 2"}
            ],
            "is_error": false
        }"#;
        let result: CallToolResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.content.len(), 2);
        assert_eq!(result.is_error, Some(false));
    }

    #[test]
    fn test_parse_call_tool_result_error() {
        let json = r#"{
            "content": [
                {"type": "text", "text": "Error: file not found"}
            ],
            "is_error": true
        }"#;
        let result: CallToolResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.is_error, Some(true));
    }

    // --- McpManager tool definition tests ---

    #[test]
    fn test_manager_empty() {
        let manager = McpManager::new();
        assert!(manager.get_tool_definitions().is_empty());
        assert_eq!(manager.status_report(), "No MCP servers connected.");
        assert!(!manager.is_mcp_tool("read"));
        assert!(manager.is_mcp_tool("mcp_fs_read_file"));
    }

    #[test]
    fn test_is_mcp_tool() {
        let manager = McpManager::new();
        assert!(manager.is_mcp_tool("mcp_filesystem_read_file"));
        assert!(manager.is_mcp_tool("mcp_remote_api_call"));
        assert!(!manager.is_mcp_tool("read"));
        assert!(!manager.is_mcp_tool("bash"));
        assert!(!manager.is_mcp_tool(""));
    }

    // --- Tool definition conversion tests (using a mock client) ---

    /// Helper to create a McpManager with pre-populated tools (no transport needed).
    fn manager_with_mock_tools(server_name: &str, tools: Vec<McpToolInfo>) -> McpManager {
        // We can't create a real McpClient without a transport, so we test
        // the conversion logic via get_tool_definitions on the manager.
        // Instead, test the conversion logic directly.
        let mut defs = Vec::new();
        for tool in &tools {
            let prefixed_name = format!("mcp_{}_{}", server_name, tool.name);
            let description = tool
                .description
                .clone()
                .unwrap_or_else(|| format!("MCP tool: {}", tool.name));
            let parameters = tool
                .input_schema
                .clone()
                .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));
            defs.push(ToolDefinition {
                tool_type: "function".into(),
                function: FunctionSchema {
                    name: prefixed_name,
                    description: format!("[MCP: {}] {}", server_name, description),
                    parameters,
                },
            });
        }
        // Return an empty manager; we test defs directly
        let _ = defs; // suppress unused warning
        McpManager::new()
    }

    #[test]
    fn test_tool_definition_conversion() {
        let tools = vec![
            McpToolInfo {
                name: "read_file".to_string(),
                description: Some("Read a file".to_string()),
                input_schema: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "File path"}
                    },
                    "required": ["path"]
                })),
            },
            McpToolInfo {
                name: "write_file".to_string(),
                description: None,
                input_schema: None,
            },
        ];

        let server_name = "filesystem";
        let mut defs = Vec::new();
        for tool in &tools {
            let prefixed_name = format!("mcp_{}_{}", server_name, tool.name);
            let description = tool
                .description
                .clone()
                .unwrap_or_else(|| format!("MCP tool: {}", tool.name));
            let parameters = tool
                .input_schema
                .clone()
                .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));
            defs.push(ToolDefinition {
                tool_type: "function".into(),
                function: FunctionSchema {
                    name: prefixed_name,
                    description: format!("[MCP: {}] {}", server_name, description),
                    parameters,
                },
            });
        }

        assert_eq!(defs.len(), 2);

        // First tool: has description and schema
        assert_eq!(defs[0].function.name, "mcp_filesystem_read_file");
        assert_eq!(
            defs[0].function.description,
            "[MCP: filesystem] Read a file"
        );
        assert_eq!(defs[0].tool_type, "function");
        let params = &defs[0].function.parameters;
        assert_eq!(params["type"], "object");
        assert!(params["properties"]["path"].is_object());

        // Second tool: no description, no schema => defaults
        assert_eq!(defs[1].function.name, "mcp_filesystem_write_file");
        assert_eq!(
            defs[1].function.description,
            "[MCP: filesystem] MCP tool: write_file"
        );
        assert_eq!(defs[1].function.parameters["type"], "object");
    }

    #[test]
    fn test_tool_name_prefixing_no_collision() {
        // Verify two servers with same tool names get distinct prefixed names
        let server_a = "alpha";
        let server_b = "beta";
        let tool = McpToolInfo {
            name: "do_thing".to_string(),
            description: Some("Does a thing".to_string()),
            input_schema: None,
        };

        let name_a = format!("mcp_{}_{}", server_a, tool.name);
        let name_b = format!("mcp_{}_{}", server_b, tool.name);

        assert_eq!(name_a, "mcp_alpha_do_thing");
        assert_eq!(name_b, "mcp_beta_do_thing");
        assert_ne!(name_a, name_b);
    }

    // --- Tool name routing tests ---

    #[test]
    fn test_tool_name_parsing() {
        // Simulate the execute_tool name parsing logic
        let prefixed = "mcp_filesystem_read_file";
        let rest = prefixed.strip_prefix("mcp_").unwrap();
        assert_eq!(rest, "filesystem_read_file");

        let server_name = "filesystem";
        let prefix = format!("{}_", server_name);
        let tool_name = rest.strip_prefix(&prefix).unwrap();
        assert_eq!(tool_name, "read_file");
    }

    #[test]
    fn test_tool_name_parsing_with_underscores() {
        // Tool name itself contains underscores
        let prefixed = "mcp_my_server_read_file_contents";
        let rest = prefixed.strip_prefix("mcp_").unwrap();

        let server_name = "my_server";
        let prefix = format!("{}_", server_name);
        let tool_name = rest.strip_prefix(&prefix).unwrap();
        assert_eq!(tool_name, "read_file_contents");
    }

    // --- Config in FullConfig tests ---

    #[test]
    fn test_full_config_with_mcp_servers() {
        let json = r#"{
            "model": "claude-opus-4-6",
            "temperature": 1.0,
            "provider": "anthropic",
            "mcp_servers": {
                "fs": {
                    "type": "local",
                    "command": ["node", "server.js"]
                },
                "api": {
                    "type": "remote",
                    "url": "https://mcp.example.com",
                    "enabled": false
                }
            },
            "config_version": 2
        }"#;
        let config: crate::config::FullConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.mcp_servers.len(), 2);
        assert!(config.mcp_servers["fs"].is_enabled());
        assert!(!config.mcp_servers["api"].is_enabled());
    }

    #[test]
    fn test_full_config_without_mcp_servers() {
        let json = r#"{
            "model": "claude-opus-4-6",
            "temperature": 1.0,
            "provider": "anthropic",
            "config_version": 2
        }"#;
        let config: crate::config::FullConfig = serde_json::from_str(json).unwrap();
        assert!(config.mcp_servers.is_empty());
    }

    // --- Mock transport for async tests ---

    struct MockTransport {
        responses: std::sync::Mutex<Vec<Result<serde_json::Value>>>,
    }

    impl MockTransport {
        fn new(responses: Vec<serde_json::Value>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses.into_iter().map(Ok).rev().collect()),
            }
        }

        fn with_error(mut responses: Vec<serde_json::Value>, error_at: usize) -> Self {
            let mut result_responses: Vec<Result<serde_json::Value>> =
                responses.drain(..).map(|v| Ok(v)).collect::<Vec<_>>();
            if error_at < result_responses.len() {
                result_responses[error_at] = Err(anyhow::anyhow!("mock transport error"));
            }
            result_responses.reverse();
            Self {
                responses: std::sync::Mutex::new(result_responses),
            }
        }
    }

    #[async_trait::async_trait]
    impl McpTransport for MockTransport {
        async fn send_request(
            &self,
            _method: &str,
            _params: Option<serde_json::Value>,
            _timeout_ms: u64,
        ) -> Result<serde_json::Value> {
            let mut responses = self.responses.lock().unwrap();
            responses
                .pop()
                .unwrap_or(Err(anyhow::anyhow!("no more mock responses")))
        }

        async fn shutdown(&self) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_mcp_client_initialize_and_discover() {
        let init_response = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "serverInfo": {"name": "test-server", "version": "1.0"}
        });
        let initialized_response = serde_json::json!({}); // notification ack
        let tools_response = serde_json::json!({
            "tools": [
                {
                    "name": "greet",
                    "description": "Say hello",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "name": {"type": "string"}
                        }
                    }
                }
            ]
        });

        let transport =
            MockTransport::new(vec![init_response, initialized_response, tools_response]);

        let mut client = McpClient {
            name: "test".to_string(),
            transport: Box::new(transport),
            tools: Vec::new(),
            timeout_ms: 5000,
        };

        client.initialize().await.unwrap();
        client.discover_tools().await.unwrap();

        assert_eq!(client.tools().len(), 1);
        assert_eq!(client.tools()[0].name, "greet");
        assert_eq!(client.tools()[0].description.as_deref(), Some("Say hello"));
        assert_eq!(client.name(), "test");
    }

    #[tokio::test]
    async fn test_mcp_client_call_tool_text() {
        let call_response = serde_json::json!({
            "content": [
                {"type": "text", "text": "Hello, World!"}
            ]
        });

        let transport = MockTransport::new(vec![call_response]);
        let client = McpClient {
            name: "test".to_string(),
            transport: Box::new(transport),
            tools: Vec::new(),
            timeout_ms: 5000,
        };

        let result = client
            .call_tool("greet", serde_json::json!({"name": "World"}))
            .await
            .unwrap();

        assert_eq!(result, "Hello, World!");
    }

    #[tokio::test]
    async fn test_mcp_client_call_tool_multi_content() {
        let call_response = serde_json::json!({
            "content": [
                {"type": "text", "text": "Line 1"},
                {"type": "text", "text": "Line 2"},
                {"type": "image", "data": "abc123", "mimeType": "image/png"},
                {"type": "resource", "resource": {"uri": "file:///x", "text": "resource text"}}
            ]
        });

        let transport = MockTransport::new(vec![call_response]);
        let client = McpClient {
            name: "test".to_string(),
            transport: Box::new(transport),
            tools: Vec::new(),
            timeout_ms: 5000,
        };

        let result = client
            .call_tool("multi", serde_json::json!({}))
            .await
            .unwrap();

        assert!(result.contains("Line 1"));
        assert!(result.contains("Line 2"));
        assert!(result.contains("[image: 6 bytes base64]"));
        assert!(result.contains("resource text"));
    }

    #[tokio::test]
    async fn test_mcp_client_call_tool_empty_content() {
        let call_response = serde_json::json!({
            "content": []
        });

        let transport = MockTransport::new(vec![call_response]);
        let client = McpClient {
            name: "test".to_string(),
            transport: Box::new(transport),
            tools: Vec::new(),
            timeout_ms: 5000,
        };

        let result = client
            .call_tool("empty", serde_json::json!({}))
            .await
            .unwrap();

        assert_eq!(result, "");
    }

    #[tokio::test]
    async fn test_mcp_client_shutdown() {
        let transport = MockTransport::new(vec![]);
        let client = McpClient {
            name: "test".to_string(),
            transport: Box::new(transport),
            tools: Vec::new(),
            timeout_ms: 5000,
        };

        // Should not error
        client.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_manager_execute_tool_not_found() {
        let manager = McpManager::new();
        let result = manager.execute_tool("mcp_nonexistent_tool", "{}").await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("No MCP server found")
        );
    }

    #[tokio::test]
    async fn test_manager_execute_tool_no_prefix() {
        let manager = McpManager::new();
        let result = manager.execute_tool("read", "{}").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("mcp_"));
    }

    #[tokio::test]
    async fn test_manager_shutdown_empty() {
        let manager = McpManager::new();
        manager.shutdown_all().await; // should not panic
    }

    #[test]
    fn test_status_report_empty() {
        let manager = McpManager::new();
        assert_eq!(manager.status_report(), "No MCP servers connected.");
    }

    // --- Integration test: full lifecycle with mock transport ---

    #[tokio::test]
    async fn test_full_lifecycle_mock() {
        // Simulate: initialize -> discover tools -> call tool -> shutdown
        let init_resp = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "serverInfo": {"name": "mock", "version": "0.1"}
        });
        let notif_resp = serde_json::json!({});
        let tools_resp = serde_json::json!({
            "tools": [
                {
                    "name": "echo",
                    "description": "Echo input back",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "message": {"type": "string"}
                        },
                        "required": ["message"]
                    }
                },
                {
                    "name": "add",
                    "description": "Add two numbers",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "a": {"type": "number"},
                            "b": {"type": "number"}
                        },
                        "required": ["a", "b"]
                    }
                }
            ]
        });
        let call_resp = serde_json::json!({
            "content": [
                {"type": "text", "text": "echoed: test message"}
            ]
        });

        let transport = MockTransport::new(vec![init_resp, notif_resp, tools_resp, call_resp]);

        let mut client = McpClient {
            name: "mock".to_string(),
            transport: Box::new(transport),
            tools: Vec::new(),
            timeout_ms: 5000,
        };

        // Initialize
        client.initialize().await.unwrap();

        // Discover tools
        client.discover_tools().await.unwrap();
        assert_eq!(client.tools().len(), 2);
        assert_eq!(client.tools()[0].name, "echo");
        assert_eq!(client.tools()[1].name, "add");

        // Call tool
        let result = client
            .call_tool("echo", serde_json::json!({"message": "test message"}))
            .await
            .unwrap();
        assert_eq!(result, "echoed: test message");

        // Shutdown
        client.shutdown().await.unwrap();
    }

    // --- Integration test with real stdio MCP server (requires npx) ---

    #[tokio::test]
    #[ignore] // Run with: cargo test -- --ignored test_real_filesystem_server
    async fn test_real_filesystem_server() {
        // This test requires npx and @modelcontextprotocol/server-filesystem
        let config = McpServerConfig::Local {
            command: vec![
                "npx".to_string(),
                "-y".to_string(),
                "@modelcontextprotocol/server-filesystem".to_string(),
                "/tmp".to_string(),
            ],
            environment: HashMap::new(),
            enabled: true,
            timeout: 30_000,
        };

        let mut servers = HashMap::new();
        servers.insert("filesystem".to_string(), config);

        let manager = McpManager::connect_all(&servers).await;
        let defs = manager.get_tool_definitions();

        // Should discover tools
        assert!(!defs.is_empty(), "Expected tools from filesystem server");

        // Check a known tool exists
        let has_list_dir = defs
            .iter()
            .any(|d| d.function.name.contains("list_directory"));
        assert!(has_list_dir, "Expected list_directory tool");

        // All tool names should be prefixed
        for def in &defs {
            assert!(
                def.function.name.starts_with("mcp_filesystem_"),
                "Tool name should be prefixed: {}",
                def.function.name
            );
        }

        // Status report should show tools
        let report = manager.status_report();
        assert!(report.contains("filesystem"));

        manager.shutdown_all().await;
    }

    #[tokio::test]
    #[ignore] // Run with: cargo test -- --ignored test_real_filesystem_tool_call
    async fn test_real_filesystem_tool_call() {
        // Create a temp file to read
        // Use canonicalize to resolve /tmp -> /private/tmp on macOS
        let tmp_dir = std::env::temp_dir().canonicalize().unwrap();
        let test_file = tmp_dir.join("bfcode_mcp_test.txt");
        std::fs::write(&test_file, "hello from mcp test").unwrap();

        let config = McpServerConfig::Local {
            command: vec![
                "npx".to_string(),
                "-y".to_string(),
                "@modelcontextprotocol/server-filesystem".to_string(),
                tmp_dir.to_string_lossy().to_string(),
            ],
            environment: HashMap::new(),
            enabled: true,
            timeout: 30_000,
        };

        let mut servers = HashMap::new();
        servers.insert("fs".to_string(), config);

        let manager = McpManager::connect_all(&servers).await;

        // Call read_file tool via the manager
        let args = serde_json::json!({"path": test_file.to_string_lossy()}).to_string();
        let result = manager.execute_tool("mcp_fs_read_file", &args).await;

        assert!(
            result.is_ok(),
            "read_file should succeed: {:?}",
            result.err()
        );
        let content = result.unwrap();
        assert!(
            content.contains("hello from mcp test"),
            "Expected file content, got: {content}"
        );

        // Cleanup
        let _ = std::fs::remove_file(&test_file);
        manager.shutdown_all().await;
    }
}
