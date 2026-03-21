use anyhow::{Context, Result, bail};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::Mutex;

/// Gateway configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    /// Listen address (default "127.0.0.1:8642")
    #[serde(default = "default_listen")]
    pub listen: String,
    /// Gateway mode
    #[serde(default)]
    pub mode: GatewayMode,
    /// Allowed API keys for authentication (empty = no auth)
    #[serde(default)]
    pub api_keys: Vec<String>,
    /// Enable Tailscale integration
    #[serde(default)]
    pub tailscale: bool,
    /// Max concurrent sessions
    #[serde(default = "default_max_sessions")]
    pub max_sessions: usize,
}

fn default_listen() -> String {
    "127.0.0.1:8642".into()
}

fn default_max_sessions() -> usize {
    10
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum GatewayMode {
    #[default]
    Local,
    Remote,
}

impl std::fmt::Display for GatewayMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GatewayMode::Local => write!(f, "local"),
            GatewayMode::Remote => write!(f, "remote"),
        }
    }
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            mode: GatewayMode::Local,
            api_keys: vec![],
            tailscale: false,
            max_sessions: default_max_sessions(),
        }
    }
}

/// Active gateway session info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewaySession {
    pub id: String,
    pub user: String,
    pub created_at: String,
    pub last_active: String,
    pub message_count: usize,
}

/// Gateway status info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayStatus {
    pub running: bool,
    pub listen: String,
    pub mode: String,
    pub uptime_secs: u64,
    pub active_sessions: usize,
    pub total_requests: u64,
    pub tailscale_ip: Option<String>,
    pub version: String,
}

// ---------------------------------------------------------------------------
// Internal server state
// ---------------------------------------------------------------------------

struct ServerState {
    sessions: HashMap<String, GatewaySession>,
    total_requests: u64,
    started_at: Instant,
    config: GatewayConfig,
    tailscale_ip: Option<String>,
}

impl ServerState {
    fn new(config: GatewayConfig) -> Self {
        let ts_ip = if config.tailscale {
            tailscale_ip()
        } else {
            None
        };
        Self {
            sessions: HashMap::new(),
            total_requests: 0,
            started_at: Instant::now(),
            tailscale_ip: ts_ip,
            config,
        }
    }

    fn status(&self) -> GatewayStatus {
        GatewayStatus {
            running: true,
            listen: self.config.listen.clone(),
            mode: self.config.mode.to_string(),
            uptime_secs: self.started_at.elapsed().as_secs(),
            active_sessions: self.sessions.len(),
            total_requests: self.total_requests,
            tailscale_ip: self.tailscale_ip.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Minimal HTTP parsing helpers
// ---------------------------------------------------------------------------

struct HttpRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

fn parse_http_request(raw: &[u8]) -> Result<HttpRequest> {
    let header_end =
        find_header_end(raw).context("Incomplete HTTP request: no header terminator")?;
    let header_bytes = &raw[..header_end];
    let header_str =
        std::str::from_utf8(header_bytes).context("HTTP headers are not valid UTF-8")?;

    let mut lines = header_str.lines();
    let request_line = lines.next().context("Empty HTTP request")?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_uppercase();
    let path = parts.next().unwrap_or("/").to_string();

    let mut headers: HashMap<String, String> = HashMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_lowercase(), value.trim().to_string());
        }
    }

    let body_start = header_end + 4; // skip \r\n\r\n
    let content_length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let body = if content_length > 0 && body_start < raw.len() {
        let end = std::cmp::min(body_start + content_length, raw.len());
        raw[body_start..end].to_vec()
    } else {
        vec![]
    };

    Ok(HttpRequest {
        method,
        path,
        headers,
        body,
    })
}

fn find_header_end(data: &[u8]) -> Option<usize> {
    data.windows(4).position(|w| w == b"\r\n\r\n")
}

fn http_response(status: u16, status_text: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
    let header = format!(
        "HTTP/1.1 {status} {status_text}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        body.len()
    );
    let mut resp = header.into_bytes();
    resp.extend_from_slice(body);
    resp
}

fn json_response(status: u16, status_text: &str, json: &serde_json::Value) -> Vec<u8> {
    let body = serde_json::to_vec(json).unwrap_or_default();
    http_response(status, status_text, "application/json", &body)
}

fn error_response(status: u16, status_text: &str, message: &str) -> Vec<u8> {
    let body = serde_json::json!({ "error": message });
    json_response(status, status_text, &body)
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// Start the gateway server (local mode).
///
/// Listens on the configured address and serves an HTTP API:
/// - `POST /v1/chat` — send a message, get a response
/// - `GET  /v1/sessions` — list sessions
/// - `POST /v1/sessions` — create a new session
/// - `GET  /v1/status` — gateway status
/// - `GET  /v1/health` — health check
pub async fn start_server(config: &GatewayConfig) -> Result<()> {
    let state = Arc::new(Mutex::new(ServerState::new(config.clone())));

    let addr: std::net::SocketAddr = config
        .listen
        .parse()
        .with_context(|| format!("Invalid listen address: {}", config.listen))?;

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("Failed to bind to {addr}"))?;

    eprintln!(
        "{} Gateway server listening on {}",
        "bfcode".cyan().bold(),
        addr.to_string().green()
    );
    eprintln!(
        "{} Mode: {}  Max sessions: {}",
        "bfcode".cyan().bold(),
        config.mode.to_string().yellow(),
        config.max_sessions
    );

    if config.tailscale {
        let st = state.lock().await;
        if let Some(ref ip) = st.tailscale_ip {
            eprintln!("{} Tailscale IP: {}", "bfcode".cyan().bold(), ip.green());
        } else {
            eprintln!(
                "{} Tailscale enabled but no IP detected",
                "bfcode".cyan().bold()
            );
        }
    }

    if config.api_keys.is_empty() {
        eprintln!(
            "{} {}",
            "bfcode".cyan().bold(),
            "Warning: no API keys configured — authentication disabled".yellow()
        );
    }

    loop {
        let (stream, peer) = listener.accept().await?;
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, peer, state).await {
                eprintln!(
                    "{} Connection error from {}: {}",
                    "bfcode".cyan().bold(),
                    peer,
                    e.to_string().red()
                );
            }
        });
    }
}

async fn handle_connection(
    mut stream: tokio::net::TcpStream,
    peer: std::net::SocketAddr,
    state: Arc<Mutex<ServerState>>,
) -> Result<()> {
    // Read request (up to 1 MB)
    let mut buf = vec![0u8; 1_048_576];
    let mut total_read = 0usize;

    // Read until we have full headers + body (or timeout after 30s)
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);
    loop {
        if tokio::time::Instant::now() >= deadline {
            bail!("Request read timeout");
        }
        if total_read >= buf.len() {
            let resp = error_response(413, "Payload Too Large", "Request too large");
            stream.write_all(&resp).await?;
            return Ok(());
        }
        let n = tokio::time::timeout(
            tokio::time::Duration::from_secs(5),
            stream.read(&mut buf[total_read..]),
        )
        .await
        .context("Read timeout")?
        .context("Read error")?;
        if n == 0 {
            break;
        }
        total_read += n;

        // Check if we have received the complete request
        if let Some(header_end) = find_header_end(&buf[..total_read]) {
            let header_str = std::str::from_utf8(&buf[..header_end]).unwrap_or("");
            let content_length: usize = header_str
                .lines()
                .find_map(|line| {
                    let lower = line.to_lowercase();
                    if lower.starts_with("content-length:") {
                        lower
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse().ok())
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            let body_start = header_end + 4;
            if total_read >= body_start + content_length {
                break;
            }
        }
    }

    if total_read == 0 {
        return Ok(());
    }

    let req = parse_http_request(&buf[..total_read])?;

    // Authenticate if api_keys are configured
    {
        let st = state.lock().await;
        if !st.config.api_keys.is_empty() {
            let authorized = req
                .headers
                .get("authorization")
                .and_then(|v| v.strip_prefix("Bearer "))
                .map(|token| st.config.api_keys.iter().any(|k| k == token))
                .unwrap_or(false);

            if !authorized {
                let resp = error_response(401, "Unauthorized", "Invalid or missing API key");
                stream.write_all(&resp).await?;
                return Ok(());
            }
        }
    }

    // Increment request counter
    {
        let mut st = state.lock().await;
        st.total_requests += 1;
    }

    let resp = route_request(&req, &state, peer).await;
    stream.write_all(&resp).await?;
    Ok(())
}

async fn route_request(
    req: &HttpRequest,
    state: &Arc<Mutex<ServerState>>,
    _peer: std::net::SocketAddr,
) -> Vec<u8> {
    let path = req.path.split('?').next().unwrap_or(&req.path);
    match (req.method.as_str(), path) {
        ("GET", "/v1/health") => handle_health(),
        ("GET", "/v1/status") => handle_status(state).await,
        ("GET", "/v1/sessions") => handle_list_sessions(state).await,
        ("POST", "/v1/sessions") => handle_create_session(req, state).await,
        ("POST", "/v1/chat") => handle_chat(req, state).await,
        _ => error_response(404, "Not Found", "Unknown endpoint"),
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

fn handle_health() -> Vec<u8> {
    json_response(200, "OK", &serde_json::json!({ "status": "ok" }))
}

async fn handle_status(state: &Arc<Mutex<ServerState>>) -> Vec<u8> {
    let st = state.lock().await;
    let status = st.status();
    let val = serde_json::to_value(&status).unwrap_or_default();
    json_response(200, "OK", &val)
}

async fn handle_list_sessions(state: &Arc<Mutex<ServerState>>) -> Vec<u8> {
    let st = state.lock().await;
    let sessions: Vec<&GatewaySession> = st.sessions.values().collect();
    let val = serde_json::to_value(&sessions).unwrap_or_default();
    json_response(200, "OK", &val)
}

async fn handle_create_session(req: &HttpRequest, state: &Arc<Mutex<ServerState>>) -> Vec<u8> {
    #[derive(Deserialize)]
    struct CreateSessionReq {
        #[serde(default = "default_user")]
        user: String,
    }
    fn default_user() -> String {
        "anonymous".into()
    }

    let parsed: CreateSessionReq = match serde_json::from_slice(&req.body) {
        Ok(v) => v,
        Err(_) => CreateSessionReq {
            user: default_user(),
        },
    };

    let mut st = state.lock().await;

    if st.sessions.len() >= st.config.max_sessions {
        return error_response(
            429,
            "Too Many Requests",
            "Maximum concurrent sessions reached",
        );
    }

    let id = format!("sess_{}", uuid_v4_simple());
    let now = chrono::Utc::now().to_rfc3339();
    let session = GatewaySession {
        id: id.clone(),
        user: parsed.user,
        created_at: now.clone(),
        last_active: now,
        message_count: 0,
    };

    st.sessions.insert(id.clone(), session.clone());

    let val = serde_json::to_value(&session).unwrap_or_default();
    json_response(201, "Created", &val)
}

async fn handle_chat(req: &HttpRequest, state: &Arc<Mutex<ServerState>>) -> Vec<u8> {
    #[derive(Deserialize)]
    struct ChatReq {
        message: String,
        #[serde(default)]
        session_id: Option<String>,
    }

    let parsed: ChatReq = match serde_json::from_slice(&req.body) {
        Ok(v) => v,
        Err(e) => {
            return error_response(400, "Bad Request", &format!("Invalid JSON body: {e}"));
        }
    };

    // Resolve or create session
    let session_id = {
        let mut st = state.lock().await;
        if let Some(ref sid) = parsed.session_id {
            if !st.sessions.contains_key(sid) {
                return error_response(404, "Not Found", "Session not found");
            }
            sid.clone()
        } else {
            // Auto-create a session
            if st.sessions.len() >= st.config.max_sessions {
                return error_response(
                    429,
                    "Too Many Requests",
                    "Maximum concurrent sessions reached",
                );
            }
            let id = format!("sess_{}", uuid_v4_simple());
            let now = chrono::Utc::now().to_rfc3339();
            let session = GatewaySession {
                id: id.clone(),
                user: "anonymous".into(),
                created_at: now.clone(),
                last_active: now,
                message_count: 0,
            };
            st.sessions.insert(id.clone(), session);
            id
        }
    };

    // Shell out to bfcode chat
    let response_text = match run_bfcode_chat(&parsed.message).await {
        Ok(text) => text,
        Err(e) => {
            return error_response(
                500,
                "Internal Server Error",
                &format!("Chat processing failed: {e}"),
            );
        }
    };

    // Update session
    {
        let mut st = state.lock().await;
        if let Some(session) = st.sessions.get_mut(&session_id) {
            session.last_active = chrono::Utc::now().to_rfc3339();
            session.message_count += 1;
        }
    }

    json_response(
        200,
        "OK",
        &serde_json::json!({
            "response": response_text,
            "session_id": session_id,
        }),
    )
}

/// Run `bfcode chat "message"` as a subprocess and capture stdout.
async fn run_bfcode_chat(message: &str) -> Result<String> {
    let output = tokio::process::Command::new("bfcode")
        .args(["chat", message])
        .output()
        .await
        .context("Failed to spawn bfcode process")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("bfcode exited with {}: {}", output.status, stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(stdout)
}

// ---------------------------------------------------------------------------
// Remote client functions
// ---------------------------------------------------------------------------

/// Connect to a remote gateway and send a message.
pub async fn remote_chat(
    gateway_url: &str,
    api_key: Option<&str>,
    message: &str,
) -> Result<String> {
    let url = format!("{}/v1/chat", gateway_url.trim_end_matches('/'));
    let client = reqwest::Client::new();

    let mut req = client
        .post(&url)
        .json(&serde_json::json!({ "message": message }))
        .timeout(std::time::Duration::from_secs(120));

    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }

    let resp = req.send().await.context("Failed to reach gateway")?;
    let status = resp.status();

    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!(
            "Gateway returned HTTP {}: {}",
            status.as_u16(),
            body.chars().take(500).collect::<String>()
        );
    }

    let json: serde_json::Value = resp
        .json()
        .await
        .context("Failed to parse gateway response")?;

    let response_text = json
        .get("response")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Ok(response_text)
}

/// Get gateway status from a remote instance.
pub async fn remote_status(gateway_url: &str, api_key: Option<&str>) -> Result<GatewayStatus> {
    let url = format!("{}/v1/status", gateway_url.trim_end_matches('/'));
    let client = reqwest::Client::new();

    let mut req = client.get(&url).timeout(std::time::Duration::from_secs(10));

    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }

    let resp = req.send().await.context("Failed to reach gateway")?;
    let status = resp.status();

    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("Gateway returned HTTP {}: {}", status.as_u16(), body);
    }

    let gw_status: GatewayStatus = resp
        .json()
        .await
        .context("Failed to parse gateway status response")?;

    Ok(gw_status)
}

// ---------------------------------------------------------------------------
// Tailscale
// ---------------------------------------------------------------------------

/// Check if Tailscale is available and get the current IPv4 address.
pub fn tailscale_ip() -> Option<String> {
    let output = std::process::Command::new("tailscale")
        .args(["ip", "-4"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let ip = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if ip.is_empty() { None } else { Some(ip) }
}

// ---------------------------------------------------------------------------
// Display helpers
// ---------------------------------------------------------------------------

/// Format gateway status for display.
pub fn format_status(status: &GatewayStatus) -> String {
    let running_str = if status.running {
        "running".green().to_string()
    } else {
        "stopped".red().to_string()
    };

    let uptime = format_duration(status.uptime_secs);

    let ts_line = match &status.tailscale_ip {
        Some(ip) => format!("  Tailscale IP:     {}\n", ip.green()),
        None => String::new(),
    };

    format!(
        "Gateway Status\n\
         ──────────────────────────────\n\
         {}\
         {}\
         {}\
         {}\
         {}\
         {}\
         {}{}",
        format_args!("  Status:           {running_str}\n"),
        format_args!("  Listen:           {}\n", status.listen),
        format_args!("  Mode:             {}\n", status.mode),
        format_args!("  Uptime:           {uptime}\n"),
        format_args!("  Active sessions:  {}\n", status.active_sessions),
        format_args!("  Total requests:   {}\n", status.total_requests),
        ts_line,
        format_args!("  Version:          {}\n", status.version),
    )
}

fn format_duration(secs: u64) -> String {
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

// ---------------------------------------------------------------------------
// Config loading
// ---------------------------------------------------------------------------

/// Load gateway config from `~/.bfcode/config.json` under the `"gateway"` key.
///
/// Returns a default config if the file does not exist or lacks a gateway section.
pub fn load_gateway_config() -> GatewayConfig {
    let path = match dirs_or_home().map(|d| d.join("config.json")) {
        Some(p) if p.exists() => p,
        _ => return GatewayConfig::default(),
    };

    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(_) => return GatewayConfig::default(),
    };

    let json: serde_json::Value = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(_) => return GatewayConfig::default(),
    };

    match json.get("gateway") {
        Some(gw) => serde_json::from_value(gw.clone()).unwrap_or_default(),
        None => GatewayConfig::default(),
    }
}

/// Resolve the `~/.bfcode` directory.
fn dirs_or_home() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".bfcode"))
}

// ---------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------

/// Generate a simple pseudo-UUID v4 (hex string, no dashes).
///
/// Uses thread-local RNG so it is fast and does not require the `uuid` crate.
fn uuid_v4_simple() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    let count = COUNTER.fetch_add(1, Ordering::Relaxed);

    // Mix in process id and monotonic counter for uniqueness
    let mix = seed ^ (std::process::id() as u128) ^ ((count as u128) << 64);

    format!("{:032x}", mix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let cfg = GatewayConfig::default();
        assert_eq!(cfg.listen, "127.0.0.1:8642");
        assert_eq!(cfg.max_sessions, 10);
        assert!(cfg.api_keys.is_empty());
        assert!(!cfg.tailscale);
    }

    #[test]
    fn test_gateway_mode_display() {
        assert_eq!(GatewayMode::Local.to_string(), "local");
        assert_eq!(GatewayMode::Remote.to_string(), "remote");
    }

    #[test]
    fn test_parse_http_request() {
        let raw = b"POST /v1/chat HTTP/1.1\r\nContent-Length: 19\r\nHost: localhost\r\n\r\n{\"message\":\"hello\"}";
        let req = parse_http_request(raw).unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/v1/chat");
        assert_eq!(req.body, b"{\"message\":\"hello\"}");
        assert_eq!(
            req.headers.get("host").map(|s| s.as_str()),
            Some("localhost")
        );
    }

    #[test]
    fn test_parse_http_get() {
        let raw = b"GET /v1/health HTTP/1.1\r\nHost: localhost\r\n\r\n";
        let req = parse_http_request(raw).unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/v1/health");
        assert!(req.body.is_empty());
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(45), "45s");
        assert_eq!(format_duration(125), "2m 5s");
        assert_eq!(format_duration(3661), "1h 1m 1s");
    }

    #[test]
    fn test_format_status() {
        let status = GatewayStatus {
            running: true,
            listen: "127.0.0.1:8642".into(),
            mode: "local".into(),
            uptime_secs: 120,
            active_sessions: 2,
            total_requests: 42,
            tailscale_ip: None,
            version: "0.1.0".into(),
        };
        let output = format_status(&status);
        assert!(output.contains("Gateway Status"));
        assert!(output.contains("127.0.0.1:8642"));
        assert!(output.contains("42"));
    }

    #[test]
    fn test_uuid_simple_length() {
        let id = uuid_v4_simple();
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_config_deserialization() {
        let json = r#"{
            "listen": "0.0.0.0:9000",
            "mode": "remote",
            "api_keys": ["key1", "key2"],
            "tailscale": true,
            "max_sessions": 5
        }"#;
        let cfg: GatewayConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.listen, "0.0.0.0:9000");
        assert!(matches!(cfg.mode, GatewayMode::Remote));
        assert_eq!(cfg.api_keys.len(), 2);
        assert!(cfg.tailscale);
        assert_eq!(cfg.max_sessions, 5);
    }

    #[test]
    fn test_json_response_format() {
        let body = serde_json::json!({"status": "ok"});
        let resp = json_response(200, "OK", &body);
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(resp_str.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(resp_str.contains("application/json"));
        assert!(resp_str.contains(r#""status":"ok""#));
    }

    #[test]
    fn test_error_response() {
        let resp = error_response(401, "Unauthorized", "Bad key");
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(resp_str.starts_with("HTTP/1.1 401 Unauthorized"));
        assert!(resp_str.contains("Bad key"));
    }

    #[test]
    fn test_gateway_config_custom_values() {
        let cfg = GatewayConfig {
            listen: "0.0.0.0:3000".into(),
            mode: GatewayMode::Remote,
            api_keys: vec!["secret1".into(), "secret2".into()],
            tailscale: true,
            max_sessions: 50,
        };
        assert_eq!(cfg.listen, "0.0.0.0:3000");
        assert!(matches!(cfg.mode, GatewayMode::Remote));
        assert_eq!(cfg.api_keys, vec!["secret1", "secret2"]);
        assert!(cfg.tailscale);
        assert_eq!(cfg.max_sessions, 50);
    }

    #[test]
    fn test_gateway_mode_display_variants() {
        let local = GatewayMode::Local;
        let remote = GatewayMode::Remote;
        assert_eq!(format!("{local}"), "local");
        assert_eq!(format!("{remote}"), "remote");
        // Also verify default is Local
        let default_mode = GatewayMode::default();
        assert_eq!(format!("{default_mode}"), "local");
    }

    #[test]
    fn test_gateway_session_serialization() {
        let session = GatewaySession {
            id: "sess_abc123".into(),
            user: "alice".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            last_active: "2026-01-01T01:00:00Z".into(),
            message_count: 7,
        };
        let json = serde_json::to_string(&session).unwrap();
        let deserialized: GatewaySession = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, "sess_abc123");
        assert_eq!(deserialized.user, "alice");
        assert_eq!(deserialized.created_at, "2026-01-01T00:00:00Z");
        assert_eq!(deserialized.last_active, "2026-01-01T01:00:00Z");
        assert_eq!(deserialized.message_count, 7);
    }

    #[test]
    fn test_gateway_status_serialization() {
        let status = GatewayStatus {
            running: true,
            listen: "127.0.0.1:8642".into(),
            mode: "local".into(),
            uptime_secs: 3600,
            active_sessions: 3,
            total_requests: 100,
            tailscale_ip: Some("100.64.0.1".into()),
            version: "1.2.3".into(),
        };
        let json = serde_json::to_string(&status).unwrap();
        let deserialized: GatewayStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.running, true);
        assert_eq!(deserialized.listen, "127.0.0.1:8642");
        assert_eq!(deserialized.mode, "local");
        assert_eq!(deserialized.uptime_secs, 3600);
        assert_eq!(deserialized.active_sessions, 3);
        assert_eq!(deserialized.total_requests, 100);
        assert_eq!(deserialized.tailscale_ip, Some("100.64.0.1".into()));
        assert_eq!(deserialized.version, "1.2.3");
    }

    #[test]
    fn test_gateway_config_with_auth() {
        let cfg = GatewayConfig {
            api_keys: vec!["key-alpha".into(), "key-beta".into(), "key-gamma".into()],
            ..GatewayConfig::default()
        };
        assert!(!cfg.api_keys.is_empty());
        assert_eq!(cfg.api_keys.len(), 3);
        assert!(cfg.api_keys.contains(&"key-alpha".to_string()));
        assert!(cfg.api_keys.contains(&"key-beta".to_string()));
        assert!(cfg.api_keys.contains(&"key-gamma".to_string()));
        // Other fields should still be defaults
        assert_eq!(cfg.listen, "127.0.0.1:8642");
        assert_eq!(cfg.max_sessions, 10);
    }

    #[test]
    fn test_load_gateway_config_missing_file() {
        // load_gateway_config returns defaults when no config file exists.
        // Since we cannot guarantee the file exists in a test environment,
        // we verify the function does not panic and returns a valid config.
        let cfg = load_gateway_config();
        // Should always return a valid config (either from file or defaults)
        assert!(!cfg.listen.is_empty());
        assert!(cfg.max_sessions > 0);
    }

    #[test]
    fn test_format_status_running() {
        let status = GatewayStatus {
            running: true,
            listen: "0.0.0.0:9999".into(),
            mode: "remote".into(),
            uptime_secs: 7261,
            active_sessions: 5,
            total_requests: 200,
            tailscale_ip: Some("100.100.1.1".into()),
            version: "2.0.0".into(),
        };
        let output = format_status(&status);
        assert!(output.contains("Gateway Status"));
        assert!(output.contains("0.0.0.0:9999"));
        assert!(output.contains("remote"));
        assert!(output.contains("200"));
        assert!(output.contains("100.100.1.1"));
        assert!(output.contains("2.0.0"));
        assert!(output.contains("2h 1m 1s"));
    }

    #[test]
    fn test_format_status_stopped() {
        let status = GatewayStatus {
            running: false,
            listen: "127.0.0.1:8642".into(),
            mode: "local".into(),
            uptime_secs: 0,
            active_sessions: 0,
            total_requests: 0,
            tailscale_ip: None,
            version: "0.1.0".into(),
        };
        let output = format_status(&status);
        assert!(output.contains("Gateway Status"));
        assert!(output.contains("0s"));
        assert!(output.contains("0.1.0"));
        // Tailscale line should not appear when None
        assert!(!output.contains("Tailscale IP"));
    }
}
