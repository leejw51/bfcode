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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener); // free the port for the gateway

    let config = bfcode::gateway::GatewayConfig {
        listen: addr.to_string(),
        max_sessions: 10,
        ..Default::default()
    };

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
