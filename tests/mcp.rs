//! Integration tests for the `mt mcp` server.
//!
//! Each test starts the server on an ephemeral port against a fresh
//! `tempdir` database, exercises a single contract (auth, MCP initialize,
//! tool list, tool call shapes), and shuts the server down. Tests never
//! share a port or a database, so they can run in parallel.

#![cfg(feature = "mcp")]

use std::net::SocketAddr;
use std::time::Duration;

use mathtutor::db::DbConfig;
use mathtutor::mcp::{self, AuthConfig};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const API_KEY: &str = "test-api-key";
const ADMIN_PASSWORD: &str = "hunter2";

struct ServerHandle {
    addr: SocketAddr,
    _tmp: TempDir,
    join: JoinHandle<()>,
}

impl ServerHandle {
    async fn stop(self) {
        self.join.abort();
        let _ = self.join.await;
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    fn issuer(&self) -> String {
        format!("http://{}", self.addr)
    }
}

/// Start `mcp::run` on an ephemeral port with the supplied `AuthConfig`.
/// Bind first to pick a free port, then close the probe listener so
/// `mcp::run` can grab the same address — the alternative is teaching
/// `run` to accept a pre-bound listener, which isn't worth the API
/// surface for tests.
async fn spawn_server_with(auth: AuthConfig) -> ServerHandle {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("mt.db");
    let cfg = DbConfig::local(db_path);

    let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = probe.local_addr().unwrap();
    drop(probe);

    let addr_str = addr.to_string();
    let join = tokio::spawn(async move {
        let _ = mcp::run(&addr_str, auth, cfg, None).await;
    });

    // Spin until the server has actually bound. Race-free enough for tests
    // because `mcp::run` binds before logging the listening message.
    for _ in 0..50 {
        if let Ok(stream) = tokio::net::TcpStream::connect(addr).await {
            drop(stream);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    ServerHandle {
        addr,
        _tmp: tmp,
        join,
    }
}

async fn spawn_server() -> ServerHandle {
    spawn_server_with(AuthConfig {
        api_key: Some(API_KEY.into()),
        ..AuthConfig::default()
    })
    .await
}

async fn spawn_oauth_server() -> ServerHandle {
    spawn_server_with(AuthConfig {
        api_key: Some(API_KEY.into()),
        admin_password: Some(ADMIN_PASSWORD.into()),
        ..AuthConfig::default()
    })
    .await
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

#[tokio::test]
async fn health_returns_200_ok_without_auth() {
    // Fly's prober won't carry a bearer; `/health` must answer 200 OK
    // unauthenticated regardless of which auth modes are configured.
    for server in [spawn_server().await, spawn_oauth_server().await] {
        let res = client().get(server.url("/health")).send().await.unwrap();
        assert_eq!(res.status(), 200);
        assert_eq!(res.text().await.unwrap(), "ok");
        server.stop().await;
    }
}

// ─────────────────────────── OAuth flow ────────────────────────────

#[tokio::test]
async fn unauthenticated_request_returns_resource_metadata_pointer() {
    // Even without OAuth enabled the 401 response should carry a
    // `WWW-Authenticate` header pointing at the protected-resource
    // metadata. That's what makes OAuth-aware MCP clients trigger the
    // connector flow on a fresh server.
    let server = spawn_oauth_server().await;
    let res = oauth_client()
        .post(server.url("/mcp"))
        .header(ACCEPT, "application/json, text/event-stream")
        .header(CONTENT_TYPE, "application/json")
        .body(initialize_body())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    let www_auth = res
        .headers()
        .get("www-authenticate")
        .expect("WWW-Authenticate header")
        .to_str()
        .unwrap();
    assert!(www_auth.starts_with("Bearer "));
    assert!(www_auth.contains("resource_metadata="));
    assert!(www_auth.contains("/.well-known/oauth-protected-resource"));
    server.stop().await;
}

#[tokio::test]
async fn protected_resource_metadata_lists_issuer() {
    let server = spawn_oauth_server().await;
    let res = oauth_client()
        .get(server.url("/.well-known/oauth-protected-resource"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["resource"].as_str().unwrap(), server.issuer());
    assert_eq!(
        body["authorization_servers"][0].as_str().unwrap(),
        server.issuer()
    );
    server.stop().await;
}

#[tokio::test]
async fn authorization_server_metadata_advertises_endpoints() {
    let server = spawn_oauth_server().await;
    let res = oauth_client()
        .get(server.url("/.well-known/oauth-authorization-server"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    let issuer = server.issuer();
    assert_eq!(body["issuer"].as_str().unwrap(), issuer);
    assert_eq!(
        body["authorization_endpoint"].as_str().unwrap(),
        format!("{issuer}/oauth/authorize")
    );
    assert_eq!(
        body["token_endpoint"].as_str().unwrap(),
        format!("{issuer}/oauth/token")
    );
    assert_eq!(
        body["registration_endpoint"].as_str().unwrap(),
        format!("{issuer}/oauth/register")
    );
    assert_eq!(
        body["code_challenge_methods_supported"][0]
            .as_str()
            .unwrap(),
        "S256",
        "PKCE S256 is OAuth 2.1 mandatory"
    );
    server.stop().await;
}

#[tokio::test]
async fn well_known_routes_unreachable_when_oauth_disabled() {
    // Bearer-only servers must not advertise discovery endpoints — there's
    // no AS to point at, and ghost metadata would confuse OAuth-aware
    // clients into trying to register.
    let server = spawn_server().await;
    let res = oauth_client()
        .get(server.url("/.well-known/oauth-authorization-server"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
    server.stop().await;
}

#[tokio::test]
async fn register_issues_client_id_and_round_trips_redirect_uris() {
    let server = spawn_oauth_server().await;
    let res = register_client(&server, "Test Client").await;
    assert!(!res.client_id.is_empty());
    assert_eq!(res.redirect_uris, vec!["http://localhost/cb".to_string()]);
    assert_eq!(res.token_endpoint_auth_method, "none");
    server.stop().await;
}

#[tokio::test]
async fn register_rejects_empty_redirect_uris() {
    let server = spawn_oauth_server().await;
    let res = oauth_client()
        .post(server.url("/oauth/register"))
        .json(&json!({ "client_name": "no redirects", "redirect_uris": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"].as_str().unwrap(), "invalid_request");
    server.stop().await;
}

#[tokio::test]
async fn full_authorization_code_flow_yields_a_working_access_token() {
    // End-to-end: DCR → /authorize GET (csrf) → /authorize POST (password,
    // get code) → /token (exchange + PKCE verify) → /mcp call.
    let server = spawn_oauth_server().await;
    let reg = register_client(&server, "Full Flow").await;

    let pkce = PkcePair::new();
    let state = "csrf-state-value";
    let auth_url = authorize_url(
        &server,
        &reg.client_id,
        &pkce.challenge,
        Some(state),
        Some("mcp"),
    );
    let form_html = oauth_client()
        .get(&auth_url)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let csrf = extract_csrf(&form_html).expect("csrf token");

    let res = oauth_client_no_redirect()
        .post(server.url("/oauth/authorize"))
        .form(&[
            ("response_type", "code"),
            ("client_id", reg.client_id.as_str()),
            ("redirect_uri", "http://localhost/cb"),
            ("code_challenge", pkce.challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", state),
            ("scope", "mcp"),
            ("resource", ""),
            ("csrf", csrf.as_str()),
            ("password", ADMIN_PASSWORD),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 303, "/authorize POST redirects on success");
    let location = res
        .headers()
        .get("location")
        .expect("location")
        .to_str()
        .unwrap()
        .to_string();
    assert!(location.starts_with("http://localhost/cb"));
    let (code, returned_state) = parse_redirect_query(&location);
    assert_eq!(returned_state.as_deref(), Some(state));

    let tokens = exchange_code(&server, &reg.client_id, &code, &pkce.verifier).await;
    assert!(!tokens.access_token.is_empty());
    assert!(!tokens.refresh_token.is_empty());
    assert_eq!(tokens.token_type, "Bearer");

    // The minted access token must satisfy /mcp without MT_API_KEY.
    let session = handshake_with_token(&server, &tokens.access_token).await;
    let res = mcp_post_with_token(
        &server,
        &session,
        &tokens.access_token,
        &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    )
    .await;
    assert_eq!(res.status(), 200);

    server.stop().await;
}

#[tokio::test]
async fn wrong_password_re_renders_form_without_issuing_code() {
    let server = spawn_oauth_server().await;
    let reg = register_client(&server, "Bad Pwd").await;
    let pkce = PkcePair::new();
    let auth_url = authorize_url(&server, &reg.client_id, &pkce.challenge, None, None);
    let form_html = oauth_client()
        .get(&auth_url)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let csrf = extract_csrf(&form_html).expect("csrf");

    let res = oauth_client_no_redirect()
        .post(server.url("/oauth/authorize"))
        .form(&[
            ("response_type", "code"),
            ("client_id", reg.client_id.as_str()),
            ("redirect_uri", "http://localhost/cb"),
            ("code_challenge", pkce.challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", ""),
            ("scope", ""),
            ("resource", ""),
            ("csrf", csrf.as_str()),
            ("password", "not the password"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    let body = res.text().await.unwrap();
    assert!(body.contains("Incorrect password"));
    server.stop().await;
}

#[tokio::test]
async fn authorization_code_is_single_use() {
    let server = spawn_oauth_server().await;
    let reg = register_client(&server, "Single Use").await;
    let pkce = PkcePair::new();
    let code = full_authorize(&server, &reg.client_id, &pkce).await;

    let first = exchange_code_raw(&server, &reg.client_id, &code, &pkce.verifier).await;
    assert_eq!(first.status(), 200, "first redemption succeeds");

    let second = exchange_code_raw(&server, &reg.client_id, &code, &pkce.verifier).await;
    assert_eq!(second.status(), 400, "second redemption rejected");
    let body: Value = second.json().await.unwrap();
    assert_eq!(body["error"].as_str().unwrap(), "invalid_grant");
    server.stop().await;
}

#[tokio::test]
async fn token_endpoint_rejects_bad_pkce_verifier() {
    let server = spawn_oauth_server().await;
    let reg = register_client(&server, "Bad PKCE").await;
    let pkce = PkcePair::new();
    let code = full_authorize(&server, &reg.client_id, &pkce).await;

    let res = exchange_code_raw(&server, &reg.client_id, &code, "wrong-verifier").await;
    assert_eq!(res.status(), 400);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"].as_str().unwrap(), "invalid_grant");
    server.stop().await;
}

#[tokio::test]
async fn refresh_token_rotates_and_returns_a_fresh_access_token() {
    let server = spawn_oauth_server().await;
    let reg = register_client(&server, "Refresh").await;
    let pkce = PkcePair::new();
    let code = full_authorize(&server, &reg.client_id, &pkce).await;
    let first = exchange_code(&server, &reg.client_id, &code, &pkce.verifier).await;

    let refresh_res = oauth_client_no_redirect()
        .post(server.url("/oauth/token"))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", first.refresh_token.as_str()),
            ("client_id", reg.client_id.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(refresh_res.status(), 200);
    let refreshed: TokenResponseDe = refresh_res.json().await.unwrap();
    assert_ne!(
        refreshed.access_token, first.access_token,
        "refresh must mint a fresh access token"
    );
    assert_ne!(
        refreshed.refresh_token, first.refresh_token,
        "refresh tokens rotate"
    );

    // The original refresh token must be invalidated after rotation.
    let replay = oauth_client_no_redirect()
        .post(server.url("/oauth/token"))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", first.refresh_token.as_str()),
            ("client_id", reg.client_id.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), 400);
    server.stop().await;
}

// ──────────────────────── OAuth test helpers ──────────────────────

fn oauth_client() -> reqwest::Client {
    client()
}

/// Reqwest client that does NOT follow redirects — we need to inspect the
/// 303 Location on the `/authorize` POST manually.
fn oauth_client_no_redirect() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

#[derive(Debug, serde::Deserialize)]
struct RegisterResponseDe {
    client_id: String,
    redirect_uris: Vec<String>,
    token_endpoint_auth_method: String,
}

async fn register_client(server: &ServerHandle, name: &str) -> RegisterResponseDe {
    oauth_client()
        .post(server.url("/oauth/register"))
        .json(&json!({
            "client_name": name,
            "redirect_uris": ["http://localhost/cb"],
        }))
        .send()
        .await
        .unwrap()
        .json::<RegisterResponseDe>()
        .await
        .unwrap()
}

#[derive(Debug, serde::Deserialize)]
struct TokenResponseDe {
    access_token: String,
    refresh_token: String,
    token_type: String,
}

struct PkcePair {
    verifier: String,
    challenge: String,
}

impl PkcePair {
    fn new() -> Self {
        use base64::Engine;
        use sha2::Digest;
        let verifier: String = (0..43)
            .map(|i| char::from(b'a' + u8::try_from(i % 26).unwrap()))
            .collect();
        let digest = sha2::Sha256::digest(verifier.as_bytes());
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
        Self {
            verifier,
            challenge,
        }
    }
}

fn extract_csrf(html: &str) -> Option<String> {
    let needle = r#"name="csrf" value=""#;
    let start = html.find(needle)? + needle.len();
    let end = start + html[start..].find('"')?;
    Some(html[start..end].to_string())
}

fn authorize_url(
    server: &ServerHandle,
    client_id: &str,
    challenge: &str,
    state: Option<&str>,
    scope: Option<&str>,
) -> String {
    let mut url = url::Url::parse(&server.url("/oauth/authorize")).unwrap();
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("response_type", "code");
        q.append_pair("client_id", client_id);
        q.append_pair("redirect_uri", "http://localhost/cb");
        q.append_pair("code_challenge", challenge);
        q.append_pair("code_challenge_method", "S256");
        if let Some(s) = state {
            q.append_pair("state", s);
        }
        if let Some(s) = scope {
            q.append_pair("scope", s);
        }
    }
    url.to_string()
}

async fn full_authorize(server: &ServerHandle, client_id: &str, pkce: &PkcePair) -> String {
    let auth_url = authorize_url(server, client_id, &pkce.challenge, None, None);
    let form_html = oauth_client()
        .get(&auth_url)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let csrf = extract_csrf(&form_html).expect("csrf");
    let res = oauth_client_no_redirect()
        .post(server.url("/oauth/authorize"))
        .form(&[
            ("response_type", "code"),
            ("client_id", client_id),
            ("redirect_uri", "http://localhost/cb"),
            ("code_challenge", pkce.challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", ""),
            ("scope", ""),
            ("resource", ""),
            ("csrf", csrf.as_str()),
            ("password", ADMIN_PASSWORD),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 303);
    let location = res
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    parse_redirect_query(&location).0
}

fn parse_redirect_query(location: &str) -> (String, Option<String>) {
    let url = url::Url::parse(location).expect("location is a URL");
    let mut code = None;
    let mut state = None;
    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            _ => {}
        }
    }
    (code.expect("code in redirect"), state)
}

async fn exchange_code_raw(
    server: &ServerHandle,
    client_id: &str,
    code: &str,
    verifier: &str,
) -> reqwest::Response {
    oauth_client_no_redirect()
        .post(server.url("/oauth/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", "http://localhost/cb"),
            ("client_id", client_id),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .unwrap()
}

async fn exchange_code(
    server: &ServerHandle,
    client_id: &str,
    code: &str,
    verifier: &str,
) -> TokenResponseDe {
    let res = exchange_code_raw(server, client_id, code, verifier).await;
    assert_eq!(res.status(), 200, "token exchange should succeed");
    res.json::<TokenResponseDe>().await.unwrap()
}

async fn handshake_with_token(server: &ServerHandle, token: &str) -> String {
    let res = client()
        .post(server.url("/mcp"))
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .header(ACCEPT, "application/json, text/event-stream")
        .header(CONTENT_TYPE, "application/json")
        .body(initialize_body())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200, "OAuth-bearer initialize must succeed");
    let session = res
        .headers()
        .get("mcp-session-id")
        .expect("session id")
        .to_str()
        .unwrap()
        .to_string();
    let _ = res.text().await;

    let res = client()
        .post(server.url("/mcp"))
        .header(AUTHORIZATION, format!("Bearer {token}"))
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
    assert!(res.status().is_success());
    let _ = res.text().await;
    session
}

async fn mcp_post_with_token(
    server: &ServerHandle,
    session: &str,
    token: &str,
    body: &Value,
) -> reqwest::Response {
    client()
        .post(server.url("/mcp"))
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .header(ACCEPT, "application/json, text/event-stream")
        .header(CONTENT_TYPE, "application/json")
        .header("mcp-session-id", session)
        .header("mcp-protocol-version", "2025-06-18")
        .body(body.to_string())
        .send()
        .await
        .unwrap()
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
