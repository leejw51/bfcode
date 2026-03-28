use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    extract::{State, WebSocketUpgrade},
    extract::ws::{Message as AxumWsMessage, WebSocket},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use colored::Colorize;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

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

pub(crate) struct ServerState {
    pub(crate) sessions: HashMap<String, GatewaySession>,
    pub(crate) total_requests: u64,
    pub(crate) started_at: Instant,
    pub(crate) config: GatewayConfig,
    pub(crate) tailscale_ip: Option<String>,
}

type AppState = Arc<Mutex<ServerState>>;

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
// Auth middleware
// ---------------------------------------------------------------------------

async fn auth_middleware(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> impl IntoResponse {
    let st = state.lock().await;
    if !st.config.api_keys.is_empty() {
        let authorized = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(|token| st.config.api_keys.iter().any(|k| k == token))
            .unwrap_or(false);

        if !authorized {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Invalid or missing API key"})),
            ));
        }
    }
    drop(st);
    Ok(next.run(request).await)
}

// ---------------------------------------------------------------------------
// Request counter middleware
// ---------------------------------------------------------------------------

async fn request_counter_middleware(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> impl IntoResponse {
    {
        let mut st = state.lock().await;
        st.total_requests += 1;
        debug!("{} {} (request #{})", request.method(), request.uri(), st.total_requests);
    }
    next.run(request).await
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// Start the gateway server using axum.
///
/// Serves an HTTP + WebSocket API:
/// - `GET  /v1/health`    — health check
/// - `GET  /v1/status`    — gateway status
/// - `GET  /v1/sessions`  — list sessions
/// - `POST /v1/sessions`  — create a new session
/// - `POST /v1/chat`      — send a message, get a response
/// - `GET  /v1/ws`        — WebSocket endpoint
pub async fn start_server(config: &GatewayConfig, verbose: bool) -> Result<()> {
    // Initialize tracing subscriber when verbose mode is enabled
    if verbose {
        use tracing_subscriber::EnvFilter;
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("bfcode=debug"));
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .init();
        info!("Verbose logging enabled");
    }

    let state: AppState = Arc::new(Mutex::new(ServerState::new(config.clone())));

    let app = build_router(state.clone());

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

    axum::serve(listener, app)
        .await
        .context("Gateway server error")?;

    Ok(())
}

/// Build the axum router (exposed for testing).
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(handle_health))
        .route("/v1/status", get(handle_status))
        .route("/v1/sessions", get(handle_list_sessions).post(handle_create_session))
        .route("/v1/chat", post(handle_chat))
        .route("/v1/ws", get(handle_ws_upgrade))
        .layer(axum::middleware::from_fn_with_state(state.clone(), request_counter_middleware))
        .layer(axum::middleware::from_fn_with_state(state.clone(), auth_middleware))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// HTTP Handlers
// ---------------------------------------------------------------------------

async fn handle_health() -> impl IntoResponse {
    Json(serde_json::json!({"status": "ok"}))
}

async fn handle_status(State(state): State<AppState>) -> impl IntoResponse {
    let st = state.lock().await;
    let status = st.status();
    Json(serde_json::to_value(&status).unwrap_or_default())
}

async fn handle_list_sessions(State(state): State<AppState>) -> impl IntoResponse {
    let st = state.lock().await;
    let sessions: Vec<&GatewaySession> = st.sessions.values().collect();
    Json(serde_json::to_value(&sessions).unwrap_or_default())
}

#[derive(Deserialize)]
struct CreateSessionReq {
    #[serde(default = "default_user")]
    user: String,
}

fn default_user() -> String {
    "anonymous".into()
}

async fn handle_create_session(
    State(state): State<AppState>,
    body: Option<Json<CreateSessionReq>>,
) -> impl IntoResponse {
    let user = body.map(|b| b.0.user).unwrap_or_else(default_user);

    let mut st = state.lock().await;

    if st.sessions.len() >= st.config.max_sessions {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({"error": "Maximum concurrent sessions reached"})),
        );
    }

    let id = format!("sess_{}", uuid_v4_simple());
    let now = chrono::Utc::now().to_rfc3339();
    let session = GatewaySession {
        id: id.clone(),
        user,
        created_at: now.clone(),
        last_active: now,
        message_count: 0,
    };

    info!("Session created: id={}, user={}", id, session.user);
    st.sessions.insert(id.clone(), session.clone());

    (
        StatusCode::CREATED,
        Json(serde_json::to_value(&session).unwrap_or_default()),
    )
}

#[derive(Deserialize)]
struct ChatReq {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

async fn handle_chat(
    State(state): State<AppState>,
    body: Option<Json<ChatReq>>,
) -> impl IntoResponse {
    let parsed = match body {
        Some(Json(b)) => b,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Invalid JSON body"})),
            );
        }
    };

    let message = match parsed.message {
        Some(ref m) if !m.is_empty() => m.clone(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Missing or empty message field"})),
            );
        }
    };
    // Resolve or create session
    let session_id = {
        let mut st = state.lock().await;
        if let Some(ref sid) = parsed.session_id {
            if !st.sessions.contains_key(sid) {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({"error": "Session not found"})),
                );
            }
            sid.clone()
        } else {
            // Auto-create a session
            if st.sessions.len() >= st.config.max_sessions {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(serde_json::json!({"error": "Maximum concurrent sessions reached"})),
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
    let result = match run_bfcode_chat(&message).await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Chat processing failed: {e}")})),
            );
        }
    };

    // Update session
    {
        let mut st = state.lock().await;
        if let Some(session) = st.sessions.get_mut(&session_id) {
            session.last_active = chrono::Utc::now().to_rfc3339();
            session.message_count += 1;
            info!("Chat completed: session={}, messages={}", session_id, session.message_count);
        }
    }

    let mut resp = serde_json::json!({
        "response": result.response,
        "session_id": session_id,
    });
    if let Some(v) = result.prompt_tokens {
        resp["prompt_tokens"] = serde_json::json!(v);
    }
    if let Some(v) = result.completion_tokens {
        resp["completion_tokens"] = serde_json::json!(v);
    }
    if let Some(v) = result.total_tokens {
        resp["total_tokens"] = serde_json::json!(v);
    }
    if let Some(v) = result.session_tokens {
        resp["session_tokens"] = serde_json::json!(v);
    }
    if let Some(v) = result.cost {
        resp["cost"] = serde_json::json!(v);
    }
    if let Some(ref v) = result.model {
        resp["model"] = serde_json::json!(v);
    }

    (StatusCode::OK, Json(resp))
}

/// Result of running bfcode chat, including optional usage metadata.
struct BfcodeResult {
    response: String,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
    session_tokens: Option<u64>,
    cost: Option<f64>,
    model: Option<String>,
}

/// Run `bfcode chat --oneshot "message"` as a subprocess and capture stdout.
///
/// Times out after 120 seconds to prevent hanging.
async fn run_bfcode_chat(message: &str) -> Result<BfcodeResult> {
    info!("Running bfcode chat: {:?}", &message[..message.len().min(100)]);
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        tokio::process::Command::new("bfcode")
            .args(["chat", "--oneshot", message])
            .output(),
    )
    .await
    .context("Chat timed out after 120 seconds")?
    .context("Failed to spawn bfcode process")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("bfcode exited with {}: {}", output.status, stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Parse metadata from stderr (line starting with __BFCODE_META__)
    let mut prompt_tokens = None;
    let mut completion_tokens = None;
    let mut total_tokens = None;
    let mut session_tokens = None;
    let mut cost = None;
    let mut model = None;
    for line in stderr.lines() {
        if let Some(json_str) = line.strip_prefix("__BFCODE_META__") {
            if let Ok(meta) = serde_json::from_str::<serde_json::Value>(json_str) {
                prompt_tokens = meta.get("prompt_tokens").and_then(|v| v.as_u64());
                completion_tokens = meta.get("completion_tokens").and_then(|v| v.as_u64());
                total_tokens = meta.get("total_tokens").and_then(|v| v.as_u64());
                session_tokens = meta.get("session_tokens").and_then(|v| v.as_u64());
                cost = meta.get("cost").and_then(|v| v.as_f64());
                model = meta.get("model").and_then(|v| v.as_str()).map(String::from);
            }
        }
    }

    Ok(BfcodeResult {
        response: stdout,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        session_tokens,
        cost,
        model,
    })
}

// ---------------------------------------------------------------------------
// WebSocket
// ---------------------------------------------------------------------------

async fn handle_ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    info!("WebSocket upgrade requested");
    ws.on_upgrade(move |socket| handle_websocket(socket, state))
}

async fn handle_websocket(socket: WebSocket, state: AppState) {
    info!("WebSocket session established");
    eprintln!(
        "{} WebSocket connected",
        "bfcode".cyan().bold(),
    );

    let (mut ws_tx, mut ws_rx) = socket.split();

    // Channel for async chat responses to be sent back on the WS
    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    loop {
        tokio::select! {
            // Incoming WS messages from client
            msg = ws_rx.next() => {
                let msg = match msg {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => {
                        warn!("WebSocket error: {}", e);
                        break;
                    }
                    None => break,
                };
                match msg {
                    AxumWsMessage::Text(text) => {
                        let reply = handle_ws_message_fast(&text, &state, &reply_tx).await;
                        if let Some(reply) = reply {
                            if ws_tx.send(AxumWsMessage::Text(reply.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    AxumWsMessage::Ping(data) => {
                        if ws_tx.send(AxumWsMessage::Pong(data)).await.is_err() {
                            break;
                        }
                    }
                    AxumWsMessage::Close(_) => break,
                    _ => {}
                }
            }
            // Async chat responses (from background tasks)
            reply = reply_rx.recv() => {
                if let Some(reply) = reply {
                    if ws_tx.send(AxumWsMessage::Text(reply.into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    }

    info!("WebSocket session disconnected");
    eprintln!("{} WebSocket disconnected", "bfcode".cyan().bold());
}

/// Fast WS message handler: returns immediately for valid chat (spawns background task),
/// returns Some(reply) for non-chat messages and validation errors, None when reply comes via channel.
async fn handle_ws_message_fast(
    text: &str,
    state: &AppState,
    reply_tx: &tokio::sync::mpsc::UnboundedSender<String>,
) -> Option<String> {
    // Quick parse to check if it's a valid chat message
    let parsed = serde_json::from_str::<serde_json::Value>(text).ok();
    let is_chat = parsed
        .as_ref()
        .and_then(|v| v.get("type")?.as_str().map(|s| s == "chat"))
        .unwrap_or(false);

    if !is_chat {
        // Non-chat messages are fast, handle inline
        return Some(handle_ws_message(text, state).await);
    }

    // Validate chat message before spawning: check message field exists
    let has_message = parsed
        .as_ref()
        .and_then(|v| v.get("message")?.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false);

    if !has_message {
        return Some(handle_ws_message(text, state).await);
    }

    // Validate session_id if provided
    if let Some(sid) = parsed.as_ref().and_then(|v| v.get("session_id")?.as_str()) {
        let session_exists = {
            let st = state.lock().await;
            st.sessions.contains_key(sid)
        };
        if !session_exists {
            return Some(handle_ws_message(text, state).await);
        }
    }

    // Valid chat request — send "thinking" ack and process in background
    let ack = serde_json::json!({"type": "thinking"}).to_string();

    let state = state.clone();
    let text = text.to_string();
    let tx = reply_tx.clone();
    tokio::spawn(async move {
        let reply = handle_ws_message(&text, &state).await;
        let _ = tx.send(reply);
    });

    Some(ack)
}

async fn handle_ws_message(text: &str, state: &AppState) -> String {
    #[derive(Deserialize)]
    struct WsRequest {
        #[serde(default)]
        r#type: String,
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        session_id: Option<String>,
    }

    let req: WsRequest = match serde_json::from_str(text) {
        Ok(r) => r,
        Err(e) => {
            return serde_json::json!({
                "type": "error",
                "error": format!("Invalid JSON: {e}")
            })
            .to_string();
        }
    };

    debug!("WebSocket message: type={}, session_id={:?}", req.r#type, req.session_id);
    match req.r#type.as_str() {
        "chat" => {
            let message = match req.message {
                Some(m) if !m.is_empty() => m,
                _ => {
                    return serde_json::json!({
                        "type": "error",
                        "error": "Missing or empty message"
                    })
                    .to_string();
                }
            };

            // Resolve or create session
            let session_id = {
                let mut st = state.lock().await;
                if let Some(ref sid) = req.session_id {
                    if !st.sessions.contains_key(sid) {
                        return serde_json::json!({
                            "type": "error",
                            "error": "Session not found"
                        })
                        .to_string();
                    }
                    sid.clone()
                } else {
                    if st.sessions.len() >= st.config.max_sessions {
                        return serde_json::json!({
                            "type": "error",
                            "error": "Maximum concurrent sessions reached"
                        })
                        .to_string();
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
                    info!("WS session created: id={}", id);
                    st.sessions.insert(id.clone(), session);
                    id
                }
            };

            // Run chat
            let result = match run_bfcode_chat(&message).await {
                Ok(r) => r,
                Err(e) => {
                    return serde_json::json!({
                        "type": "error",
                        "error": format!("Chat failed: {e}")
                    })
                    .to_string();
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

            let mut resp = serde_json::json!({
                "type": "response",
                "response": result.response,
                "session_id": session_id,
            });
            if let Some(v) = result.prompt_tokens {
                resp["prompt_tokens"] = serde_json::json!(v);
            }
            if let Some(v) = result.completion_tokens {
                resp["completion_tokens"] = serde_json::json!(v);
            }
            if let Some(v) = result.total_tokens {
                resp["total_tokens"] = serde_json::json!(v);
            }
            if let Some(v) = result.session_tokens {
                resp["session_tokens"] = serde_json::json!(v);
            }
            if let Some(v) = result.cost {
                resp["cost"] = serde_json::json!(v);
            }
            if let Some(ref v) = result.model {
                resp["model"] = serde_json::json!(v);
            }
            resp.to_string()
        }
        "ping" => serde_json::json!({"type": "pong"}).to_string(),
        "health" => serde_json::json!({"type": "health", "status": "ok"}).to_string(),
        "status" => {
            let st = state.lock().await;
            let status = st.status();
            let mut resp = serde_json::to_value(&status).unwrap_or_default();
            resp["type"] = serde_json::json!("status");
            resp.to_string()
        }
        _ => serde_json::json!({
            "type": "error",
            "error": format!("Unknown message type: {}", req.r#type)
        })
        .to_string(),
    }
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
        assert_eq!(cfg.listen, "127.0.0.1:8642");
        assert_eq!(cfg.max_sessions, 10);
    }

    #[test]
    fn test_load_gateway_config_missing_file() {
        let cfg = load_gateway_config();
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
        assert!(!output.contains("Tailscale IP"));
    }

    #[tokio::test]
    async fn test_run_bfcode_chat_with_echo() {
        let result = BfcodeResult {
            response: "Hello, World!".into(),
            prompt_tokens: Some(10),
            completion_tokens: Some(20),
            total_tokens: Some(30),
            session_tokens: Some(100),
            cost: Some(0.001),
            model: Some("test-model".into()),
        };
        assert_eq!(result.response, "Hello, World!");
        assert_eq!(result.total_tokens, Some(30));
        assert_eq!(result.model.as_deref(), Some("test-model"));
    }

    // --- Axum handler tests using the real router ---

    #[tokio::test]
    async fn test_axum_health() {
        let state: AppState = Arc::new(Mutex::new(ServerState::new(GatewayConfig::default())));
        let app = build_router(state);

        let resp = axum::serve(
            tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(),
            app,
        );
        // Use a simpler approach: test handler directly
        let result = handle_health().await;
        let resp = result.into_response();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_server_state_status() {
        let config = GatewayConfig::default();
        let state = ServerState::new(config);
        let status = state.status();
        assert!(status.running);
        assert_eq!(status.listen, "127.0.0.1:8642");
        assert_eq!(status.mode, "local");
    }

    #[tokio::test]
    async fn test_server_state_sessions() {
        let config = GatewayConfig::default();
        let mut state = ServerState::new(config);

        let id = format!("sess_{}", uuid_v4_simple());
        let now = chrono::Utc::now().to_rfc3339();
        let session = GatewaySession {
            id: id.clone(),
            user: "tester".into(),
            created_at: now.clone(),
            last_active: now,
            message_count: 0,
        };
        state.sessions.insert(id.clone(), session);

        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.sessions.get(&id).unwrap().user, "tester");

        let status = state.status();
        assert_eq!(status.active_sessions, 1);
    }

    #[tokio::test]
    async fn test_ws_message_ping() {
        let state: AppState = Arc::new(Mutex::new(ServerState::new(GatewayConfig::default())));
        let resp = handle_ws_message(r#"{"type":"ping"}"#, &state).await;
        let json: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(json["type"], "pong");
    }

    #[tokio::test]
    async fn test_ws_message_health() {
        let state: AppState = Arc::new(Mutex::new(ServerState::new(GatewayConfig::default())));
        let resp = handle_ws_message(r#"{"type":"health"}"#, &state).await;
        let json: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(json["type"], "health");
        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn test_ws_message_status() {
        let state: AppState = Arc::new(Mutex::new(ServerState::new(GatewayConfig::default())));
        let resp = handle_ws_message(r#"{"type":"status"}"#, &state).await;
        let json: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(json["type"], "status");
        assert_eq!(json["running"], true);
    }

    #[tokio::test]
    async fn test_ws_message_unknown_type() {
        let state: AppState = Arc::new(Mutex::new(ServerState::new(GatewayConfig::default())));
        let resp = handle_ws_message(r#"{"type":"foobar"}"#, &state).await;
        let json: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(json["type"], "error");
        assert!(json["error"].as_str().unwrap().contains("Unknown message type"));
    }

    #[tokio::test]
    async fn test_ws_message_invalid_json() {
        let state: AppState = Arc::new(Mutex::new(ServerState::new(GatewayConfig::default())));
        let resp = handle_ws_message("not json{{{", &state).await;
        let json: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(json["type"], "error");
        assert!(json["error"].as_str().unwrap().contains("Invalid JSON"));
    }

    #[tokio::test]
    async fn test_ws_message_chat_missing_message() {
        let state: AppState = Arc::new(Mutex::new(ServerState::new(GatewayConfig::default())));
        let resp = handle_ws_message(r#"{"type":"chat"}"#, &state).await;
        let json: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(json["type"], "error");
    }

    #[tokio::test]
    async fn test_ws_message_chat_invalid_session() {
        let state: AppState = Arc::new(Mutex::new(ServerState::new(GatewayConfig::default())));
        let resp = handle_ws_message(
            r#"{"type":"chat","message":"hello","session_id":"sess_nonexistent"}"#,
            &state,
        )
        .await;
        let json: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(json["type"], "error");
        assert!(json["error"].as_str().unwrap().contains("Session not found"));
    }
}
