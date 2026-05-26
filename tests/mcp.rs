//! Integration tests for the `mt mcp` server.
//!
//! Each test starts the server on an ephemeral port against a fresh
//! `tempdir` database, exercises a single contract (auth, MCP initialize,
//! tool list, tool call shapes), and shuts the server down. Tests never
//! share a port or a database, so they can run in parallel.

#![cfg(feature = "mcp")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::{Router, middleware};
use mathtutor::db::{self, DbConfig};
use mathtutor::mcp::{self, MathTutorServer};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const API_KEY: &str = "test-api-key";

struct ServerHandle {
    addr: SocketAddr,
    _tmp: TempDir,
    shutdown: CancellationToken,
    join: JoinHandle<()>,
}

impl ServerHandle {
    async fn stop(self) {
        self.shutdown.cancel();
        let _ = self.join.await;
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }
}

/// Build a server bound to an ephemeral port. We construct the router /
/// service directly (mirroring `mcp::run`) instead of going through
/// `mcp::run` so the test owns the cancellation token and the bound port.
async fn spawn_server() -> ServerHandle {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("mt.db");
    let cfg = DbConfig::local(db_path);
    let db = Arc::new(db::open(&cfg).await.expect("open db"));
    let cfg_arc = Arc::new(cfg);

    let shutdown = CancellationToken::new();
    let mcp_config = StreamableHttpServerConfig::default()
        .with_sse_keep_alive(Some(Duration::from_secs(15)))
        .with_cancellation_token(shutdown.child_token())
        .disable_allowed_hosts();

    let factory = {
        let db = db.clone();
        let cfg = cfg_arc.clone();
        move || Ok(MathTutorServer::new(db.clone(), cfg.clone(), None))
    };

    let service = StreamableHttpService::new(
        factory,
        Arc::new(LocalSessionManager::default()),
        mcp_config,
    );

    let app = Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(
            mcp::test_support::AuthState::new(API_KEY),
            mcp::test_support::auth_middleware,
        ));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let shutdown_clone = shutdown.clone();
    let join = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move { shutdown_clone.cancelled().await })
            .await;
    });

    ServerHandle {
        addr,
        _tmp: tmp,
        shutdown,
        join,
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

#[tokio::test]
async fn rejects_missing_api_key_with_401() {
    let server = spawn_server().await;
    let res = client()
        .post(server.url("/mcp"))
        .header(ACCEPT, "application/json, text/event-stream")
        .header(CONTENT_TYPE, "application/json")
        .body(initialize_body())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    server.stop().await;
}

#[tokio::test]
async fn rejects_wrong_api_key_with_401() {
    let server = spawn_server().await;
    let res = client()
        .post(server.url("/mcp"))
        .header(AUTHORIZATION, "Bearer wrong-token")
        .header(ACCEPT, "application/json, text/event-stream")
        .header(CONTENT_TYPE, "application/json")
        .body(initialize_body())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    server.stop().await;
}

#[tokio::test]
async fn accepts_query_token_when_header_absent() {
    // EventSource-style clients can't set headers, so the query-string
    // fallback is the only way they authenticate. Drive the same initialize
    // request through `?token=` and make sure the MCP handshake completes.
    let server = spawn_server().await;
    let url = format!("{}?token={API_KEY}", server.url("/mcp"));
    let res = client()
        .post(&url)
        .header(ACCEPT, "application/json, text/event-stream")
        .header(CONTENT_TYPE, "application/json")
        .body(initialize_body())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    server.stop().await;
}

#[tokio::test]
async fn initialize_returns_server_info_and_capabilities() {
    let server = spawn_server().await;
    let res = client()
        .post(server.url("/mcp"))
        .header(AUTHORIZATION, format!("Bearer {API_KEY}"))
        .header(ACCEPT, "application/json, text/event-stream")
        .header(CONTENT_TYPE, "application/json")
        .body(initialize_body())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let session = res
        .headers()
        .get("mcp-session-id")
        .expect("session id header")
        .to_str()
        .unwrap()
        .to_string();
    let body = res.text().await.unwrap();
    let json = parse_sse_message(&body).expect("sse json");
    let result = json
        .get("result")
        .expect("initialize result")
        .as_object()
        .unwrap();
    assert_eq!(
        result["protocolVersion"].as_str().unwrap(),
        "2025-06-18",
        "server advertises the MCP version we wired in"
    );
    assert!(
        result["capabilities"].get("tools").is_some(),
        "tools capability advertised"
    );
    assert!(
        result["capabilities"].get("prompts").is_some(),
        "prompts capability advertised"
    );
    assert_eq!(
        result["serverInfo"]["name"].as_str().unwrap(),
        "rmcp",
        "name comes from rmcp's build env helper"
    );

    assert!(!session.is_empty());
    server.stop().await;
}

#[tokio::test]
async fn tools_list_includes_every_documented_tool() {
    // Initialize, capture the session id, then call `tools/list` and check
    // every tool in the design doc is present (snake_case names match the
    // method names in `mcp.rs`).
    let server = spawn_server().await;
    let session = handshake(&server).await;

    let res = mcp_post(
        &server,
        &session,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
        }),
    )
    .await;
    assert_eq!(res.status(), 200);
    let body = res.text().await.unwrap();
    let msg = parse_sse_message(&body).expect("sse json");

    let names: Vec<String> = msg["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    let expected = [
        "get_paths",
        "new_path",
        "get_next",
        "get_state",
        "get_tree",
        "get_item",
        "get_children",
        "upsert_lesson",
        "create_quiz",
        "update_quiz",
        "delete_quiz",
        "answer_quiz",
    ];
    for tool in &expected {
        assert!(
            names.contains(&(*tool).to_string()),
            "tools/list missing {tool}; got {names:?}",
        );
    }
    server.stop().await;
}

#[tokio::test]
async fn prompts_list_includes_playbook() {
    let server = spawn_server().await;
    let session = handshake(&server).await;

    let res = mcp_post(
        &server,
        &session,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "prompts/list",
        }),
    )
    .await;
    assert_eq!(res.status(), 200);
    let body = res.text().await.unwrap();
    let msg = parse_sse_message(&body).expect("sse json");
    let names: Vec<String> = msg["result"]["prompts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, vec!["mathtutor-playbook"]);
    server.stop().await;
}

#[tokio::test]
async fn get_paths_returns_empty_list_on_fresh_db() {
    // First end-to-end tool invocation: prove the path from JSON-RPC
    // through `tool_router` through `list_paths` returns a sensible JSON
    // body with `isError: false`.
    let server = spawn_server().await;
    let session = handshake(&server).await;

    let res = mcp_post(
        &server,
        &session,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "get_paths",
                "arguments": {}
            }
        }),
    )
    .await;
    assert_eq!(res.status(), 200);
    let body = res.text().await.unwrap();
    let msg = parse_sse_message(&body).expect("sse json");
    let result = &msg["result"];
    // Old MCP omitted `isError` on success; new MCP sets it to `false`.
    // Either way it must not be `true`.
    assert_ne!(
        result["isError"].as_bool(),
        Some(true),
        "tool should succeed"
    );
    // MCP `structuredContent` must be a JSON object — Claude Desktop
    // rejects bare arrays. The empty path list is wrapped as
    // `{"paths": []}`.
    assert_eq!(
        result["structuredContent"],
        json!({ "paths": [] }),
        "structured content should be an object wrapping the path list"
    );
    server.stop().await;
}

#[tokio::test]
async fn unknown_atom_id_returns_structured_business_error() {
    // Domain failures (atom not found, no path, etc.) must come back as
    // `isError: true` JSON, not as a JSON-RPC protocol error — that's
    // what the design doc tells the agent to recover from.
    let server = spawn_server().await;
    let session = handshake(&server).await;

    let res = mcp_post(
        &server,
        &session,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "new_path",
                "arguments": {
                    "goal": "Test",
                    "atoms": ["no.such.atom.exists"]
                }
            }
        }),
    )
    .await;
    assert_eq!(res.status(), 200);
    let body = res.text().await.unwrap();
    let msg = parse_sse_message(&body).expect("sse json");
    let result = &msg["result"];
    assert_eq!(
        result["isError"].as_bool(),
        Some(true),
        "expected structured error tool result"
    );
    let structured = &result["structuredContent"];
    assert_eq!(structured["kind"].as_str(), Some("unknown_id"));
    assert!(
        structured["error"]
            .as_str()
            .unwrap()
            .contains("no.such.atom.exists")
    );
    server.stop().await;
}

// ─────────────────────────── helpers ───────────────────────────

fn initialize_body() -> String {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "mt-test", "version": "0"}
        }
    })
    .to_string()
}

async fn handshake(server: &ServerHandle) -> String {
    let res = client()
        .post(server.url("/mcp"))
        .header(AUTHORIZATION, format!("Bearer {API_KEY}"))
        .header(ACCEPT, "application/json, text/event-stream")
        .header(CONTENT_TYPE, "application/json")
        .body(initialize_body())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200, "initialize must succeed");
    let session = res
        .headers()
        .get("mcp-session-id")
        .expect("session id")
        .to_str()
        .unwrap()
        .to_string();
    // Drain the body so the SSE stream closes.
    let _ = res.text().await;

    // MCP requires the client to send `notifications/initialized` before
    // calling other methods. The body is accepted with 202 (no SSE).
    let res = client()
        .post(server.url("/mcp"))
        .header(AUTHORIZATION, format!("Bearer {API_KEY}"))
        .header(ACCEPT, "application/json, text/event-stream")
        .header(CONTENT_TYPE, "application/json")
        .header("mcp-session-id", &session)
        .header("mcp-protocol-version", "2025-06-18")
        .body(
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            })
            .to_string(),
        )
        .send()
        .await
        .unwrap();
    assert!(res.status().is_success(), "initialized notification");
    let _ = res.text().await;

    session
}

async fn mcp_post(server: &ServerHandle, session: &str, body: &Value) -> reqwest::Response {
    client()
        .post(server.url("/mcp"))
        .header(AUTHORIZATION, format!("Bearer {API_KEY}"))
        .header(ACCEPT, "application/json, text/event-stream")
        .header(CONTENT_TYPE, "application/json")
        .header("mcp-session-id", session)
        .header("mcp-protocol-version", "2025-06-18")
        .body(body.to_string())
        .send()
        .await
        .unwrap()
}

/// Parse one JSON-RPC message out of an SSE response body. SSE chunks
/// look like `event: message\ndata: {...}\n\n`; we want the JSON in the
/// `data:` line. (The streamable-http transport may send a priming event
/// first; skip past anything that doesn't deserialize as JSON.)
fn parse_sse_message(body: &str) -> Option<Value> {
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            let trimmed = rest.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(trimmed)
                && (v.get("result").is_some() || v.get("error").is_some())
            {
                return Some(v);
            }
        }
    }
    None
}
