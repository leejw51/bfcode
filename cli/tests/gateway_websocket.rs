//! WebSocket integration tests for the bfcode gateway server.
//!
//! These tests spin up the gateway server on a random port and connect
//! via WebSocket to exercise the WS protocol (ping, health, status, chat,
//! session management, and error paths).
//!
//! Run:
//!   cargo test --test gateway_websocket

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// Start the gateway server on a random available port and return the address.
async fn start_test_server() -> std::net::SocketAddr {
    start_test_server_with_config(bfcode::gateway::GatewayConfig {
        max_sessions: 10,
        ..Default::default()
    })
    .await
}

/// Start the gateway server with custom config on a random port.
async fn start_test_server_with_config(
    mut config: bfcode::gateway::GatewayConfig,
) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener); // free the port for the gateway

    config.listen = addr.to_string();

    tokio::spawn(async move {
        bfcode::gateway::start_server(&config, false).await.unwrap();
    });

    // Give the server a moment to bind
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    addr
}

/// Connect a WebSocket client to the test server.
async fn ws_connect(
    addr: std::net::SocketAddr,
) -> (
    futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        WsMessage,
    >,
    futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
) {
    let url = format!("ws://{}/v1/ws", addr);
    let (ws_stream, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws_stream.split()
}

/// Send a JSON message and read the JSON response.
async fn send_recv(
    tx: &mut futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        WsMessage,
    >,
    rx: &mut futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    msg: serde_json::Value,
) -> serde_json::Value {
    tx.send(WsMessage::Text(msg.to_string().into()))
        .await
        .unwrap();
    let resp = tokio::time::timeout(std::time::Duration::from_secs(5), rx.next())
        .await
        .expect("Timeout waiting for WS response")
        .expect("Stream ended")
        .expect("WS error");
    match resp {
        WsMessage::Text(text) => serde_json::from_str(&text).expect("Invalid JSON response"),
        other => panic!("Expected Text message, got: {:?}", other),
    }
}

// ============================================================
// Ping / Pong
// ============================================================

#[tokio::test]
async fn test_ws_ping_message() {
    let addr = start_test_server().await;
    let (mut tx, mut rx) = ws_connect(addr).await;

    let resp = send_recv(
        &mut tx,
        &mut rx,
        serde_json::json!({"type": "ping"}),
    )
    .await;

    assert_eq!(resp["type"], "pong");
}

// ============================================================
// Health
// ============================================================

#[tokio::test]
async fn test_ws_health() {
    let addr = start_test_server().await;
    let (mut tx, mut rx) = ws_connect(addr).await;

    let resp = send_recv(
        &mut tx,
        &mut rx,
        serde_json::json!({"type": "health"}),
    )
    .await;

    assert_eq!(resp["type"], "health");
    assert_eq!(resp["status"], "ok");
}

// ============================================================
// Status
// ============================================================

#[tokio::test]
async fn test_ws_status() {
    let addr = start_test_server().await;
    let (mut tx, mut rx) = ws_connect(addr).await;

    let resp = send_recv(
        &mut tx,
        &mut rx,
        serde_json::json!({"type": "status"}),
    )
    .await;

    assert_eq!(resp["type"], "status");
    assert_eq!(resp["running"], true);
    assert!(resp["listen"].as_str().is_some());
}

// ============================================================
// Unknown message type → error
// ============================================================

#[tokio::test]
async fn test_ws_unknown_type_returns_error() {
    let addr = start_test_server().await;
    let (mut tx, mut rx) = ws_connect(addr).await;

    let resp = send_recv(
        &mut tx,
        &mut rx,
        serde_json::json!({"type": "foobar"}),
    )
    .await;

    assert_eq!(resp["type"], "error");
    let err = resp["error"].as_str().unwrap();
    assert!(err.contains("Unknown message type"));
}

// ============================================================
// Invalid JSON → error
// ============================================================

#[tokio::test]
async fn test_ws_invalid_json_returns_error() {
    let addr = start_test_server().await;
    let (mut tx, mut rx) = ws_connect(addr).await;

    tx.send(WsMessage::Text("not valid json{{{".into()))
        .await
        .unwrap();

    let resp = tokio::time::timeout(std::time::Duration::from_secs(5), rx.next())
        .await
        .expect("Timeout")
        .expect("Stream ended")
        .expect("WS error");

    match resp {
        WsMessage::Text(text) => {
            let json: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(json["type"], "error");
            assert!(json["error"].as_str().unwrap().contains("Invalid JSON"));
        }
        other => panic!("Expected Text, got: {:?}", other),
    }
}

// ============================================================
// Chat with missing message → error
// ============================================================

#[tokio::test]
async fn test_ws_chat_missing_message() {
    let addr = start_test_server().await;
    let (mut tx, mut rx) = ws_connect(addr).await;

    let resp = send_recv(
        &mut tx,
        &mut rx,
        serde_json::json!({"type": "chat"}),
    )
    .await;

    assert_eq!(resp["type"], "error");
    let err = resp["error"].as_str().unwrap();
    assert!(err.contains("Missing") || err.contains("empty"));
}

// ============================================================
// Chat with non-existent session → error
// ============================================================

#[tokio::test]
async fn test_ws_chat_invalid_session() {
    let addr = start_test_server().await;
    let (mut tx, mut rx) = ws_connect(addr).await;

    let resp = send_recv(
        &mut tx,
        &mut rx,
        serde_json::json!({
            "type": "chat",
            "message": "hello",
            "session_id": "sess_nonexistent"
        }),
    )
    .await;

    assert_eq!(resp["type"], "error");
    let err = resp["error"].as_str().unwrap();
    assert!(err.contains("Session not found"));
}

// ============================================================
// Multiple messages on one connection
// ============================================================

#[tokio::test]
async fn test_ws_multiple_messages_same_connection() {
    let addr = start_test_server().await;
    let (mut tx, mut rx) = ws_connect(addr).await;

    // First: ping
    let resp1 = send_recv(&mut tx, &mut rx, serde_json::json!({"type": "ping"})).await;
    assert_eq!(resp1["type"], "pong");

    // Second: health
    let resp2 = send_recv(&mut tx, &mut rx, serde_json::json!({"type": "health"})).await;
    assert_eq!(resp2["type"], "health");

    // Third: status
    let resp3 = send_recv(&mut tx, &mut rx, serde_json::json!({"type": "status"})).await;
    assert_eq!(resp3["type"], "status");
}

// ============================================================
// WebSocket close
// ============================================================

#[tokio::test]
async fn test_ws_graceful_close() {
    let addr = start_test_server().await;
    let (mut tx, mut rx) = ws_connect(addr).await;

    // Send a ping to verify connection works
    let resp = send_recv(&mut tx, &mut rx, serde_json::json!({"type": "ping"})).await;
    assert_eq!(resp["type"], "pong");

    // Close the connection
    tx.send(WsMessage::Close(None)).await.unwrap();

    // Next read should indicate close or end of stream
    let next = tokio::time::timeout(std::time::Duration::from_secs(2), rx.next()).await;
    match next {
        Ok(Some(Ok(WsMessage::Close(_)))) | Ok(None) | Err(_) => {
            // Expected: server responds with close frame or stream ends
        }
        Ok(Some(Ok(msg))) => {
            // Some servers may send close frame back
            assert!(
                matches!(msg, WsMessage::Close(_)),
                "Expected Close, got: {:?}",
                msg
            );
        }
        Ok(Some(Err(_))) => {
            // Connection error after close is acceptable
        }
    }
}

// ============================================================
// HTTP endpoints still work alongside WebSocket
// ============================================================

#[tokio::test]
async fn test_http_health_endpoint() {
    let addr = start_test_server().await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{}/v1/health", addr))
        .send()
        .await
        .unwrap();

    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn test_http_status_endpoint() {
    let addr = start_test_server().await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{}/v1/status", addr))
        .send()
        .await
        .unwrap();

    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["running"], true);
}

#[tokio::test]
async fn test_http_create_and_list_sessions() {
    let addr = start_test_server().await;
    let client = reqwest::Client::new();

    // Create session
    let resp = client
        .post(format!("http://{}/v1/sessions", addr))
        .json(&serde_json::json!({"user": "ws_tester"}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["id"].as_str().unwrap().starts_with("sess_"));
    assert_eq!(body["user"], "ws_tester");

    // List sessions
    let resp = client
        .get(format!("http://{}/v1/sessions", addr))
        .send()
        .await
        .unwrap();

    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    let sessions = body.as_array().unwrap();
    assert!(!sessions.is_empty());
    assert!(sessions.iter().any(|s| s["user"] == "ws_tester"));
}

// ============================================================
// HTTP 404 for unknown endpoint
// ============================================================

#[tokio::test]
async fn test_http_unknown_endpoint_returns_404() {
    let addr = start_test_server().await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{}/v1/nonexistent", addr))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 404);
}

// ============================================================
// Heartbeat tests
// ============================================================

/// Helper: start a server with fast heartbeat (1s interval, 2s timeout).
async fn start_fast_heartbeat_server() -> std::net::SocketAddr {
    start_test_server_with_config(bfcode::gateway::GatewayConfig {
        max_sessions: 10,
        heartbeat_interval_secs: 1,
        heartbeat_timeout_secs: 2,
        ..Default::default()
    })
    .await
}

/// Server sends Ping frames; client that responds with Pong stays connected.
#[tokio::test]
async fn test_heartbeat_ping_received() {
    let addr = start_fast_heartbeat_server().await;
    let (mut tx, mut rx) = ws_connect(addr).await;

    // Wait for a server Ping (should arrive within ~1s)
    let msg = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Some(Ok(msg)) = rx.next().await {
                if matches!(msg, WsMessage::Ping(_)) {
                    return msg;
                }
            }
        }
    })
    .await
    .expect("Should receive a Ping from server within 3s");

    assert!(matches!(msg, WsMessage::Ping(_)));

    // Respond with Pong to keep alive
    if let WsMessage::Ping(data) = msg {
        tx.send(WsMessage::Pong(data)).await.unwrap();
    }

    // Verify connection is still alive by sending a message
    let resp = send_recv(&mut tx, &mut rx, serde_json::json!({"type": "ping"})).await;
    assert_eq!(resp["type"], "pong");
}

/// Client responds to Pong — connection stays alive across multiple heartbeats.
#[tokio::test]
async fn test_heartbeat_keeps_connection_alive() {
    let addr = start_fast_heartbeat_server().await;
    let (mut tx, mut rx) = ws_connect(addr).await;

    // Respond to pings for ~3 heartbeat cycles
    let survived = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut pong_count = 0u32;
        loop {
            match rx.next().await {
                Some(Ok(WsMessage::Ping(data))) => {
                    tx.send(WsMessage::Pong(data)).await.unwrap();
                    pong_count += 1;
                    if pong_count >= 3 {
                        return pong_count;
                    }
                }
                Some(Ok(WsMessage::Close(_))) | None => {
                    panic!("Connection closed after {} pongs", pong_count);
                }
                _ => continue,
            }
        }
    })
    .await
    .expect("Should survive 3 heartbeat cycles");

    assert!(survived >= 3);

    // Connection should still work
    let resp = send_recv(&mut tx, &mut rx, serde_json::json!({"type": "health"})).await;
    assert_eq!(resp["type"], "health");
}

/// Client ignores Pings (no Pong) — server closes connection after timeout.
///
/// Note: tokio-tungstenite auto-responds to protocol-level Ping frames,
/// so we test this by connecting via raw WebSocket handshake and reading
/// frames without sending Pong back.
#[tokio::test]
async fn test_heartbeat_timeout_closes_connection() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let addr = start_fast_heartbeat_server().await;

    // Connect via raw TCP and perform WebSocket handshake manually
    let mut tcp = tokio::net::TcpStream::connect(addr).await.unwrap();

    let handshake = format!(
        "GET /v1/ws HTTP/1.1\r\n\
         Host: {}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
         Sec-WebSocket-Version: 13\r\n\
         \r\n",
        addr
    );
    tcp.write_all(handshake.as_bytes()).await.unwrap();

    // Read handshake response (just consume it)
    let mut buf = vec![0u8; 4096];
    let n = tcp.read(&mut buf).await.unwrap();
    let response = String::from_utf8_lossy(&buf[..n]);
    assert!(response.contains("101"), "Expected 101 Switching Protocols, got: {}", response);

    // Now just wait — do NOT send any Pong frames.
    // The server should close the connection after the heartbeat timeout (~2s).
    let closed = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let mut frame_buf = vec![0u8; 1024];
            match tcp.read(&mut frame_buf).await {
                Ok(0) => return true,  // Connection closed
                Err(_) => return true, // Connection error = closed
                Ok(n) => {
                    // Check if we got a Close frame (opcode 0x08)
                    if n >= 2 && (frame_buf[0] & 0x0F) == 0x08 {
                        return true;
                    }
                    // Otherwise keep reading (might be Ping frames we ignore)
                    continue;
                }
            }
        }
    })
    .await
    .expect("Server should close connection within 10s due to heartbeat timeout");

    assert!(closed, "Server should have closed the connection due to heartbeat timeout");
}

/// WebSocket Ping frame (protocol-level) gets a Pong response.
#[tokio::test]
async fn test_ws_protocol_ping_pong() {
    let addr = start_test_server().await;
    let (mut tx, mut rx) = ws_connect(addr).await;

    // Send a protocol-level Ping
    tx.send(WsMessage::Ping(b"hello".to_vec().into()))
        .await
        .unwrap();

    // Should get a Pong back with the same payload
    let resp = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Some(Ok(msg)) = rx.next().await {
                if let WsMessage::Pong(data) = msg {
                    return data;
                }
            }
        }
    })
    .await
    .expect("Should receive Pong within 3s");

    assert_eq!(&resp[..], b"hello");
}
