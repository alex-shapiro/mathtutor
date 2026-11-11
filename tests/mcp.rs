//! Integration tests for the `mt mcp` server.

#![cfg(feature = "mcp")]

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use mathtutor::mcp::{self, AuthConfig};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const API_KEY: &str = "test-api-key-0123456789abcdefghij";
const ADMIN_PASSWORD: &str = "hunter2";

struct ServerHandle {
    addr: SocketAddr,
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

// Probe an ephemeral port first so `mcp::run` can bind to a known address.
async fn spawn_server_with(auth: AuthConfig) -> ServerHandle {
    spawn_server_on(common::fresh_config(), auth).await
}

async fn spawn_server_on(cfg: mathtutor::db::DbConfig, auth: AuthConfig) -> ServerHandle {
    common::create_db(&cfg).await;
    let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = probe.local_addr().unwrap();
    drop(probe);

    let addr_str = addr.to_string();
    let join = tokio::spawn(async move {
        let _ = mcp::run(&addr_str, auth, cfg).await;
    });

    for _ in 0..250 {
        if let Ok(stream) = tokio::net::TcpStream::connect(addr).await {
            drop(stream);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    ServerHandle { addr, join }
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
async fn server_stops_at_startup_without_creating_a_missing_database() {
    let cfg = common::fresh_config();
    let auth = AuthConfig {
        api_key: Some(API_KEY.into()),
        ..AuthConfig::default()
    };

    let e = mcp::run("127.0.0.1:0", auth, cfg.clone())
        .await
        .unwrap_err();

    let message = e.to_string();
    assert!(
        matches!(e, mathtutor::Error::DatabaseNotFound { .. }),
        "{e:?}"
    );
    assert!(message.contains("BASERUNNA_DB"), "{message}");
    assert!(message.contains("BASERUNNA_URL"), "{message}");
    assert!(!message.contains("mt path new"), "{message}");
    let raw = baserunna_client::Client::new(cfg.transport().unwrap()).with_token(&cfg.token);
    let info = raw.db(&cfg.db).info().await;
    assert!(
        info.is_err(),
        "the server must not create the database: {info:?}"
    );
}

#[tokio::test]
async fn database_failure_during_token_check_returns_503_without_a_login_challenge() {
    let cfg = common::fresh_config();
    let server = spawn_server_on(
        cfg.clone(),
        AuthConfig {
            api_key: Some(API_KEY.into()),
            admin_password: Some(ADMIN_PASSWORD.into()),
            ..AuthConfig::default()
        },
    )
    .await;
    baserunna_client::Client::new(cfg.transport().unwrap())
        .with_token(&cfg.token)
        .db(&cfg.db)
        .delete()
        .await
        .unwrap();

    let resp = client()
        .post(server.url("/mcp"))
        .bearer_auth(ISSUED_FORM_TOKEN)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert!(resp.headers().contains_key("retry-after"));
    assert!(!resp.headers().contains_key("www-authenticate"));

    let malformed = client()
        .post(server.url("/mcp"))
        .bearer_auth("an-oauth-access-token")
        .send()
        .await
        .unwrap();
    assert_eq!(
        malformed.status(),
        401,
        "a malformed token fails without a database request"
    );
    server.stop().await;
}

/// Token with the form of an issued access token that the server did not issue.
const ISSUED_FORM_TOKEN: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

#[tokio::test]
async fn failed_bearer_tokens_delay_the_next_request_from_the_address() {
    let server = spawn_oauth_server().await;
    let post = async |token: &str| {
        client()
            .post(server.url("/mcp"))
            .bearer_auth(token)
            .header(ACCEPT, "application/json, text/event-stream")
            .header(CONTENT_TYPE, "application/json")
            .body(initialize_body())
            .send()
            .await
            .unwrap()
    };
    for token in [ISSUED_FORM_TOKEN, "wrong"].repeat(3).into_iter().take(5) {
        assert_eq!(post(token).await.status(), 401);
    }

    let res = post(API_KEY).await;
    assert_eq!(res.status(), 429, "the correct key must wait too");
    assert_eq!(res.headers()["retry-after"], "1");

    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(post(API_KEY).await.status(), 200, "the wait ends");
    server.stop().await;
}

#[tokio::test]
async fn accepts_query_token_if_header_absent() {
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
    assert!(
        res.headers().get("mcp-session-id").is_none(),
        "the server keeps no session"
    );
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

    server.stop().await;
}

#[tokio::test]
async fn tools_list_includes_every_documented_tool() {
    let server = spawn_server().await;

    let res = mcp_post(
        &server,
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
        "get_syllabus",
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
async fn get_next_advertises_mode_enum_in_schema() {
    let server = spawn_server().await;

    let res = mcp_post(
        &server,
        &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    )
    .await;
    assert_eq!(res.status(), 200);
    let body = res.text().await.unwrap();
    let msg = parse_sse_message(&body).expect("sse json");
    let get_next = msg["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"].as_str() == Some("get_next"))
        .expect("get_next tool");
    let schema = &get_next["inputSchema"];
    assert!(
        !schema["properties"]["mode"].is_null(),
        "`mode` property missing from inputSchema",
    );
    let rendered = schema.to_string();
    assert!(
        rendered.contains("\"new\""),
        "schema missing `new` variant: {rendered}",
    );
    assert!(
        rendered.contains("\"due\""),
        "schema missing `due` variant: {rendered}",
    );
    server.stop().await;
}

#[tokio::test]
async fn prompts_list_includes_playbook() {
    let server = spawn_server().await;

    let res = mcp_post(
        &server,
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
    let server = spawn_server().await;

    let res = mcp_post(
        &server,
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
    assert_ne!(
        result["isError"].as_bool(),
        Some(true),
        "tool should succeed"
    );
    assert_eq!(
        result["structuredContent"],
        json!({ "paths": [] }),
        "structured content should be an object wrapping the path list"
    );
    server.stop().await;
}

#[tokio::test]
async fn server_caches_the_overlay_only_in_memory() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = mathtutor::db::DbConfig {
        cache_dir: Some(dir.path().to_path_buf()),
        ..common::fresh_config()
    };
    let server = spawn_server_on(
        cfg,
        AuthConfig {
            api_key: Some(API_KEY.into()),
            ..AuthConfig::default()
        },
    )
    .await;

    let result = call_tool(&server, "get_paths", json!({})).await;

    assert_eq!(result["structuredContent"], json!({ "paths": [] }));
    let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert!(files.is_empty(), "{files:?}");
    server.stop().await;
}

#[tokio::test]
async fn unknown_atom_id_returns_structured_business_error() {
    let server = spawn_server().await;

    let res = mcp_post(
        &server,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "new_path",
                "arguments": {
                    "goal": "Test",
                    "targets": ["no.such.atom.exists"]
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
async fn get_syllabus_returns_upcoming_topics_for_new_path() {
    let server = spawn_server().await;

    let res = mcp_post(
        &server,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "new_path",
                "arguments": { "goal": "Test", "targets": ["fnd.1.1.2"] }
            }
        }),
    )
    .await;
    let msg = parse_sse_message(&res.text().await.unwrap()).expect("sse");
    let path_id = msg["result"]["structuredContent"]["path_id"]
        .as_str()
        .expect("path_id")
        .to_string();

    let res = mcp_post(
        &server,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "get_syllabus",
                "arguments": { "path_id": path_id, "n": 10 }
            }
        }),
    )
    .await;
    let msg = parse_sse_message(&res.text().await.unwrap()).expect("sse");
    let structured = &msg["result"]["structuredContent"];
    assert_ne!(msg["result"]["isError"].as_bool(), Some(true));
    assert_eq!(structured["schema_version"], json!(1));
    assert_eq!(structured["path"], json!(path_id));
    assert_eq!(structured["total_remaining"], json!(2));
    let atoms = structured["atoms"].as_array().expect("atoms array");
    let ids: Vec<&str> = atoms.iter().map(|a| a["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["fnd.1.1.1", "fnd.1.1.2"]);
    server.stop().await;
}

/// Call tool `name` with `arguments` and return its result.
async fn call_tool(server: &ServerHandle, name: &str, arguments: Value) -> Value {
    let res = mcp_post(
        server,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        }),
    )
    .await;
    let msg = parse_sse_message(&res.text().await.unwrap()).expect("sse");
    msg["result"].clone()
}

#[tokio::test]
async fn path_tools_return_the_cli_views_with_mcp_next_steps() {
    let server = spawn_server().await;
    let created = call_tool(
        &server,
        "new_path",
        json!({ "goal": "Test", "targets": ["fnd.1.1.2"] }),
    )
    .await;
    let path_id = created["structuredContent"]["path_id"].clone();

    let paths = call_tool(&server, "get_paths", json!({})).await;
    let summary = &paths["structuredContent"]["paths"][0];
    assert_eq!(summary["id"], path_id);
    assert_eq!(
        summary["targets"],
        json!({ "total": 1, "learned": 0, "learned_pct": 0 })
    );

    let next = call_tool(&server, "get_next", json!({ "path_id": path_id })).await;
    let step = next["structuredContent"]["next_step"].as_str().unwrap();
    assert!(step.contains(&format!("path_id: {path_id}")), "{step}");
    assert!(!step.starts_with("mt "), "{step}");

    let tree = call_tool(&server, "get_tree", json!({ "path_id": path_id })).await;
    let tree = &tree["structuredContent"];
    assert_eq!(tree["next"], json!("fnd.1.1.1"));
    assert_eq!(tree["reachable"]["total"], json!(2));
    assert_eq!(tree["areas"][0]["prefix"], json!("fnd"));
    server.stop().await;
}

#[tokio::test]
async fn a_second_server_on_the_same_database_answers_without_an_initialize() {
    let cfg = common::fresh_config();
    let auth = AuthConfig {
        api_key: Some(API_KEY.into()),
        ..AuthConfig::default()
    };
    let first = spawn_server_on(cfg.clone(), auth.clone()).await;
    let created = call_tool(
        &first,
        "new_path",
        json!({ "goal": "Test", "targets": ["fnd.1.1.2"] }),
    )
    .await;
    let path_id = created["structuredContent"]["path_id"].clone();
    first.stop().await;

    let second = spawn_server_on(cfg, auth).await;
    let next = call_tool(&second, "get_next", json!({ "path_id": path_id })).await;
    assert_ne!(next["isError"], json!(true), "{next}");
    assert_eq!(next["structuredContent"]["path"], path_id);
    second.stop().await;
}

#[tokio::test]
async fn quiz_tools_accept_the_shared_quiz_fields() {
    let server = spawn_server().await;
    let created = call_tool(
        &server,
        "new_path",
        json!({ "goal": "Test", "targets": ["fnd.1.1.2"], "strategy": "top_down" }),
    )
    .await;
    let path_id = created["structuredContent"]["path_id"].clone();
    let lesson = json!({ "atom": "fnd.1.1.2", "body": "b", "path_id": path_id });
    call_tool(&server, "upsert_lesson", lesson).await;

    let quiz = json!({
        "atom": "fnd.1.1.2",
        "difficulty": "easy",
        "question": "q",
        "answer": "a",
        "path_id": path_id,
    });
    let created = call_tool(&server, "create_quiz", quiz).await;
    assert_ne!(created["isError"], json!(true), "{created}");
    let quiz_id = created["structuredContent"]["quiz_id"].clone();
    assert_eq!(quiz_id, json!("fnd.1.1.2.q1"));

    let patch = json!({ "quiz_id": quiz_id, "question": "q2", "path_id": path_id });
    let updated = call_tool(&server, "update_quiz", patch).await;
    assert_ne!(updated["isError"], json!(true), "{updated}");
    let next = call_tool(&server, "get_next", json!({ "path_id": path_id })).await;
    let quiz = &next["structuredContent"]["payload"]["quiz"];
    assert_eq!(
        (&quiz["question"], &quiz["answer"]),
        (&json!("q2"), &json!("a"))
    );
    server.stop().await;
}

#[tokio::test]
async fn health_returns_200_ok_without_auth() {
    for server in [spawn_server().await, spawn_oauth_server().await] {
        let res = client().get(server.url("/health")).send().await.unwrap();
        assert_eq!(res.status(), 200);
        assert_eq!(res.text().await.unwrap(), "ok");
        server.stop().await;
    }
}

#[tokio::test]
async fn ready_reports_a_usable_database_without_auth() {
    let server = spawn_server().await;
    let res = client().get(server.url("/ready")).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.text().await.unwrap(), "ready");
    server.stop().await;
}

#[tokio::test]
async fn ready_fails_while_health_passes_if_the_database_is_gone() {
    let cfg = common::fresh_config();
    let server = spawn_server_on(
        cfg.clone(),
        AuthConfig {
            api_key: Some(API_KEY.into()),
            ..AuthConfig::default()
        },
    )
    .await;
    baserunna_client::Client::new(cfg.transport().unwrap())
        .with_token(&cfg.token)
        .db(&cfg.db)
        .delete()
        .await
        .unwrap();

    let res = client().get(server.url("/ready")).send().await.unwrap();
    assert_eq!(res.status(), 503);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body, json!({ "ready": false, "kind": "config" }));
    let health = client().get(server.url("/health")).send().await.unwrap();
    assert_eq!(health.status(), 200, "the machine stays alive");
    server.stop().await;
}

#[tokio::test]
async fn unauthenticated_request_returns_resource_metadata_pointer() {
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
async fn well_known_routes_return_404_if_oauth_disabled() {
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
    assert_ne!(res.client_id, "");
    assert_eq!(res.redirect_uris, vec!["http://localhost/cb".to_string()]);
    assert_eq!(res.token_endpoint_auth_method, "none");
    server.stop().await;
}

#[tokio::test]
async fn register_rejects_empty_redirect_uris() {
    let server = spawn_oauth_server().await;
    let res = oauth_client()
        .post(server.url("/oauth/register"))
        .json(&json!({ "client_name": "empty redirects", "redirect_uris": [] }))
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
    let res = submit_login(&server, &form_html, ADMIN_PASSWORD).await;
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
    assert_ne!(tokens.access_token, "");
    assert_ne!(tokens.refresh_token, "");
    assert_eq!(tokens.token_type, "Bearer");

    let res = mcp_post_with_token(
        &server,
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
    let res = submit_login(&server, &form_html, "not the password").await;
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

async fn refresh_raw(
    server: &ServerHandle,
    client_id: &str,
    refresh_token: &str,
    scope: Option<&str>,
) -> reqwest::Response {
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
    ];
    form.extend(scope.map(|s| ("scope", s)));
    oauth_client_no_redirect()
        .post(server.url("/oauth/token"))
        .form(&form)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn sign_in_without_scope_or_state_grants_mcp_and_adds_no_state() {
    let server = spawn_oauth_server().await;
    let reg = register_client(&server, "No Scope").await;
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
    let names: Vec<String> = hidden_fields(&form_html)
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    for absent in ["state", "scope", "resource"] {
        assert!(!names.iter().any(|n| n == absent), "{absent} in {names:?}");
    }

    let res = submit_login(&server, &form_html, ADMIN_PASSWORD).await;
    assert_eq!(res.status(), 303);
    let location = res.headers()["location"].to_str().unwrap().to_string();
    let (code, state) = parse_redirect_query(&location);
    assert_eq!(state, None, "the redirect adds a state: {location}");

    let res = exchange_code_raw(&server, &reg.client_id, &code, &pkce.verifier).await;
    assert_eq!(res.status(), 200);
    let tokens: Value = res.json().await.unwrap();
    assert_eq!(tokens["scope"], "mcp");
    server.stop().await;
}

#[tokio::test]
async fn refresh_that_names_mcp_keeps_a_grant_without_a_requested_scope() {
    let server = spawn_oauth_server().await;
    let reg = register_client(&server, "Scope").await;
    let pkce = PkcePair::new();
    let code = full_authorize(&server, &reg.client_id, &pkce).await;
    let first = exchange_code(&server, &reg.client_id, &code, &pkce.verifier).await;

    let res = refresh_raw(&server, &reg.client_id, &first.refresh_token, Some("mcp")).await;
    assert_eq!(res.status(), 200);
    let refreshed: Value = res.json().await.unwrap();
    assert_eq!(refreshed["scope"], "mcp");
    let access = refreshed["access_token"].as_str().unwrap();
    let list_tools = json!({"jsonrpc":"2.0","id":2,"method":"tools/list"});
    let res = mcp_post_with_token(&server, access, &list_tools).await;
    assert_eq!(res.status(), 200);
    server.stop().await;
}

#[tokio::test]
async fn refresh_rejects_a_scope_other_than_mcp() {
    let server = spawn_oauth_server().await;
    let reg = register_client(&server, "Scope").await;
    let pkce = PkcePair::new();
    let code = full_authorize(&server, &reg.client_id, &pkce).await;
    let first = exchange_code(&server, &reg.client_id, &code, &pkce.verifier).await;

    let broader = refresh_raw(
        &server,
        &reg.client_id,
        &first.refresh_token,
        Some("mcp admin"),
    )
    .await;
    assert_eq!(broader.status(), 400);
    let body: Value = broader.json().await.unwrap();
    assert_eq!(body["error"], "invalid_scope");
    server.stop().await;
}

#[tokio::test]
async fn authorize_rejects_an_unknown_scope() {
    let server = spawn_oauth_server().await;
    let reg = register_client(&server, "Unknown Scope").await;
    let pkce = PkcePair::new();
    let auth_url = authorize_url(
        &server,
        &reg.client_id,
        &pkce.challenge,
        None,
        Some("admin"),
    );
    let res = oauth_client().get(&auth_url).send().await.unwrap();
    assert_eq!(res.status(), 400);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"], "invalid_scope");
    server.stop().await;
}

#[tokio::test]
async fn stored_codes_and_tokens_are_keyed_by_the_hash_of_the_secret() {
    use mathtutor::db::{Op, Row};
    use mathtutor::mcp::oauth::secret_hash;
    use mathtutor::schema::{OAuthAccessToken, OAuthCode, OAuthRefreshToken};

    let cfg = common::fresh_config();
    let server = spawn_server_on(
        cfg.clone(),
        AuthConfig {
            api_key: Some(API_KEY.into()),
            admin_password: Some(ADMIN_PASSWORD.into()),
            ..AuthConfig::default()
        },
    )
    .await;
    let reg = register_client(&server, "Hashed").await;
    let pkce = PkcePair::new();
    let code = full_authorize(&server, &reg.client_id, &pkce).await;
    let raw = baserunna_client::Client::new(cfg.transport().unwrap())
        .with_token(&cfg.token)
        .db(&cfg.db);
    let codes = async || -> Vec<OAuthCode> {
        let page = raw.read_one(Op::query_all::<OAuthCode>()).await.unwrap();
        page.rows().unwrap()
    };
    let codes_before_exchange = codes().await;
    let tokens = exchange_code(&server, &reg.client_id, &code, &pkce.verifier).await;
    assert!(codes().await.is_empty(), "the exchange deletes the code");

    let rows = raw
        .read(&[
            Op::query_all::<OAuthAccessToken>(),
            Op::query_all::<OAuthRefreshToken>(),
        ])
        .await
        .unwrap();
    let [access, refresh]: [_; 2] = rows.try_into().unwrap();
    let access: Vec<OAuthAccessToken> = access.rows().unwrap();
    let refresh: Vec<OAuthRefreshToken> = refresh.rows().unwrap();
    let stored = [
        (
            OAuthCode::TABLE,
            codes_before_exchange[0].code_hash.as_str(),
            code.as_str(),
        ),
        (
            OAuthAccessToken::TABLE,
            access[0].token_hash.as_str(),
            tokens.access_token.as_str(),
        ),
        (
            OAuthRefreshToken::TABLE,
            refresh[0].token_hash.as_str(),
            tokens.refresh_token.as_str(),
        ),
    ];
    for (table, key, secret) in stored {
        assert_ne!(key, secret, "{table} stores the secret");
        assert_eq!(key, secret_hash(secret), "{table}");
    }
    let res = mcp_post_with_token(
        &server,
        &tokens.access_token,
        &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    )
    .await;
    assert_eq!(res.status(), 200, "the access token still authenticates");
    server.stop().await;
}

fn oauth_client() -> reqwest::Client {
    client()
}

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

/// Return the name and value of every hidden input of the login form `html`.
fn hidden_fields(html: &str) -> Vec<(String, String)> {
    let unescape = |v: &str| {
        v.replace("&quot;", "\"")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&")
    };
    html.split("<input type=\"hidden\" ")
        .skip(1)
        .map(|input| {
            let attr = |name: &str| {
                let needle = format!("{name}=\"");
                let start = input.find(&needle).expect("attribute") + needle.len();
                let end = start + input[start..].find('"').expect("closing quote");
                unescape(&input[start..end])
            };
            (attr("name"), attr("value"))
        })
        .collect()
}

/// Post the hidden inputs of the login form `html` with `password`, as a browser does.
async fn submit_login(server: &ServerHandle, html: &str, password: &str) -> reqwest::Response {
    let mut fields = hidden_fields(html);
    fields.push(("password".into(), password.into()));
    oauth_client_no_redirect()
        .post(server.url("/oauth/authorize"))
        .form(&fields)
        .send()
        .await
        .unwrap()
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
    let res = submit_login(server, &form_html, ADMIN_PASSWORD).await;
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

async fn mcp_post_with_token(
    server: &ServerHandle,
    token: &str,
    body: &Value,
) -> reqwest::Response {
    client()
        .post(server.url("/mcp"))
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .header(ACCEPT, "application/json, text/event-stream")
        .header(CONTENT_TYPE, "application/json")
        .header("mcp-protocol-version", "2025-06-18")
        .body(body.to_string())
        .send()
        .await
        .unwrap()
}

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

async fn mcp_post(server: &ServerHandle, body: &Value) -> reqwest::Response {
    mcp_post_with_token(server, API_KEY, body).await
}

// Skip non-JSON SSE priming events and return the first JSON-RPC payload.
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

#[tokio::test]
async fn item_lookup_without_a_path_reports_authored_lessons() {
    let server = spawn_server().await;
    let created = call_tool(
        &server,
        "new_path",
        json!({ "goal": "Test", "targets": ["fnd.1.1.2"] }),
    )
    .await;
    let path_id = created["structuredContent"]["path_id"].clone();
    let item = json!({ "id": "fnd.1.1.2" });
    let before = call_tool(&server, "get_item", item.clone()).await;
    assert_eq!(before["structuredContent"]["has_lesson"], json!(false));

    let lesson = json!({ "atom": "fnd.1.1.2", "body": "b", "path_id": path_id });
    call_tool(&server, "upsert_lesson", lesson).await;
    let after = call_tool(&server, "get_item", item).await;
    assert_eq!(after["structuredContent"]["has_lesson"], json!(true));
    server.stop().await;
}

#[tokio::test]
async fn repeated_incorrect_passwords_delay_the_next_sign_in() {
    let server = spawn_oauth_server().await;
    let reg = register_client(&server, "Guesser").await;
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
    for _ in 0..5 {
        let res = submit_login(&server, &form_html, "wrong").await;
        assert_eq!(res.status(), 401);
    }

    let res = submit_login(&server, &form_html, ADMIN_PASSWORD).await;
    assert_eq!(res.status(), 429, "the correct password must wait too");
    assert_eq!(res.headers()["retry-after"], "1");
    assert!(res.text().await.unwrap().contains("Try again in 1 second."));

    tokio::time::sleep(Duration::from_secs(1)).await;
    let res = submit_login(&server, &form_html, ADMIN_PASSWORD).await;
    assert_eq!(res.status(), 303, "the wait ends after the delay");
    server.stop().await;
}

#[tokio::test]
async fn the_sign_in_delay_and_an_open_form_survive_a_server_restart() {
    let cfg = common::fresh_config();
    let auth = AuthConfig {
        admin_password: Some(ADMIN_PASSWORD.into()),
        ..AuthConfig::default()
    };
    let first = spawn_server_on(cfg.clone(), auth.clone()).await;
    let reg = register_client(&first, "Restarted").await;
    let pkce = PkcePair::new();
    let auth_url = authorize_url(&first, &reg.client_id, &pkce.challenge, None, None);
    let form_html = oauth_client()
        .get(&auth_url)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    for _ in 0..5 {
        assert_eq!(
            submit_login(&first, &form_html, "wrong").await.status(),
            401
        );
    }
    first.stop().await;

    let second = spawn_server_on(cfg, auth).await;
    let res = submit_login(&second, &form_html, ADMIN_PASSWORD).await;
    assert_eq!(res.status(), 429, "the restart keeps the delay");

    tokio::time::sleep(Duration::from_secs(1)).await;
    let res = submit_login(&second, &form_html, ADMIN_PASSWORD).await;
    assert_eq!(res.status(), 303, "the restart keeps the form valid");
    second.stop().await;
}

#[tokio::test]
async fn an_expired_sign_in_form_shows_a_new_form() {
    let server = spawn_oauth_server().await;
    let reg = register_client(&server, "Stale").await;
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
    let stale_csrf = hidden_fields(&form_html)
        .into_iter()
        .find(|(name, _)| name == "csrf")
        .unwrap()
        .1;
    let stale_html = form_html.replace(&stale_csrf, "1.AAAA");

    let res = submit_login(&server, &stale_html, ADMIN_PASSWORD).await;

    assert_eq!(res.status(), 400);
    assert!(
        res.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    let page = res.text().await.unwrap();
    assert!(page.contains("This sign-in form expired."), "{page}");
    let res = submit_login(&server, &page, ADMIN_PASSWORD).await;
    assert_eq!(res.status(), 303, "the new form signs in");
    server.stop().await;
}

#[tokio::test]
async fn the_sign_in_page_names_the_client_and_the_redirect_host() {
    let server = spawn_oauth_server().await;
    let redirect = "https://connector.example:8443/callback";
    let reg: RegisterResponseDe = oauth_client()
        .post(server.url("/oauth/register"))
        .json(&json!({
            "client_name": "Claude <script>",
            "redirect_uris": [redirect],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pkce = PkcePair::new();
    let mut url = url::Url::parse(&server.url("/oauth/authorize")).unwrap();
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &reg.client_id)
        .append_pair("redirect_uri", redirect)
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256");

    let page = oauth_client()
        .get(url.as_str())
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(
        page.contains("<strong>Claude &lt;script&gt;</strong>"),
        "{page}"
    );
    assert!(
        page.contains("<strong>connector.example:8443</strong>"),
        "{page}"
    );
    assert!(!page.contains("<script>"), "{page}");
    server.stop().await;
}

#[tokio::test]
async fn registrations_from_one_address_are_limited() {
    let server = spawn_oauth_server().await;
    for i in 0..10 {
        register_client(&server, &format!("client {i}")).await;
    }
    let res = oauth_client()
        .post(server.url("/oauth/register"))
        .json(&json!({ "redirect_uris": ["http://localhost/cb"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 429);
    assert!(res.headers().contains_key("retry-after"));
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"], "too_many_requests");
    server.stop().await;
}

#[tokio::test]
async fn a_token_grant_keeps_the_client_registration_for_the_refresh_token_lifetime() {
    let cfg = common::fresh_config();
    let server = spawn_server_on(
        cfg.clone(),
        AuthConfig {
            admin_password: Some(ADMIN_PASSWORD.into()),
            ..AuthConfig::default()
        },
    )
    .await;
    let reg = register_client(&server, "Expiring").await;
    let days_left = async || {
        let res: Value = client()
            .post(format!("{}/v1/dbs/{}/txn", cfg.url, cfg.db))
            .bearer_auth(&cfg.token)
            .json(&json!({
                "ops": [{ "op": "get_row", "table": "oauth_clients", "pk": reg.client_id }],
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let expires_at = res["results"][0]["expires_at"].as_i64().expect("ttl");
        let day = 86_400_000;
        (expires_at - chrono::Utc::now().timestamp_millis() + day / 2) / day
    };
    assert_eq!(days_left().await, 1, "an unused registration expires");

    let pkce = PkcePair::new();
    let code = full_authorize(&server, &reg.client_id, &pkce).await;
    let tokens = exchange_code(&server, &reg.client_id, &code, &pkce.verifier).await;
    assert_eq!(days_left().await, 30, "a code exchange renews the client");

    let res = refresh_raw(&server, &reg.client_id, &tokens.refresh_token, None).await;
    assert_eq!(res.status(), 200, "a refresh renews the client again");
    assert_eq!(days_left().await, 30);
    server.stop().await;
}

#[tokio::test]
async fn a_validated_access_token_needs_no_further_database_read() {
    use mathtutor::db::Op;
    use mathtutor::mcp::oauth::secret_hash;
    use mathtutor::schema::OAuthAccessToken;

    let cfg = common::fresh_config();
    let server = spawn_server_on(
        cfg.clone(),
        AuthConfig {
            admin_password: Some(ADMIN_PASSWORD.into()),
            ..AuthConfig::default()
        },
    )
    .await;
    let reg = register_client(&server, "Cached").await;
    let pkce = PkcePair::new();
    let code = full_authorize(&server, &reg.client_id, &pkce).await;
    let tokens = exchange_code(&server, &reg.client_id, &code, &pkce.verifier).await;
    let list_tools = json!({"jsonrpc":"2.0","id":2,"method":"tools/list"});
    let first = mcp_post_with_token(&server, &tokens.access_token, &list_tools).await;
    assert_eq!(first.status(), 200);

    let raw = baserunna_client::Client::new(cfg.transport().unwrap())
        .with_token(&cfg.token)
        .db(&cfg.db);
    let hash = secret_hash(&tokens.access_token);
    raw.write(&[Op::delete_row::<OAuthAccessToken>(&hash)])
        .await
        .unwrap();

    let res = mcp_post_with_token(&server, &tokens.access_token, &list_tools).await;
    assert_eq!(res.status(), 200, "the server reads the token from memory");
    server.stop().await;
}

#[tokio::test]
async fn new_path_accepts_a_target_inside_another_target() {
    let server = spawn_server().await;
    let created = call_tool(
        &server,
        "new_path",
        json!({ "goal": "Overlap", "targets": ["la.5", "la.5.4"] }),
    )
    .await;
    assert_ne!(created["isError"], json!(true), "{created}");
    let path_id = created["structuredContent"]["path_id"].clone();
    let next = call_tool(&server, "get_next", json!({ "path_id": path_id })).await;
    assert_ne!(next["isError"], json!(true), "{next}");
    server.stop().await;
}

#[tokio::test]
async fn sign_in_and_token_requests_from_one_address_are_limited() {
    let server = spawn_oauth_server().await;
    let token_request = || {
        oauth_client()
            .post(server.url("/oauth/token"))
            .form(&[("grant_type", "password")])
            .send()
    };
    for _ in 0..60 {
        assert_eq!(token_request().await.unwrap().status(), 400);
    }

    let res = token_request().await.unwrap();
    assert_eq!(res.status(), 429);
    assert!(res.headers().contains_key("retry-after"));
    let res = oauth_client()
        .get(server.url("/oauth/authorize?client_id=x"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 429, "the sign-in form shares the limit");
    server.stop().await;
}

#[tokio::test]
async fn only_the_routes_for_browser_clients_allow_other_origins() {
    let server = spawn_oauth_server().await;
    let reg = register_client(&server, "Origin").await;
    let pkce = PkcePair::new();
    let auth_url = authorize_url(&server, &reg.client_id, &pkce.challenge, None, None);
    let origin = "https://elsewhere.example";
    let allowed =
        |res: &reqwest::Response| res.headers().contains_key("access-control-allow-origin");

    let form = oauth_client()
        .get(&auth_url)
        .header("origin", origin)
        .send()
        .await
        .unwrap();
    assert_eq!(form.status(), 200);
    assert!(!allowed(&form), "the sign-in form allows another origin");

    for path in [
        "/.well-known/oauth-authorization-server",
        "/.well-known/oauth-protected-resource",
    ] {
        let res = oauth_client()
            .get(server.url(path))
            .header("origin", origin)
            .send()
            .await
            .unwrap();
        assert!(allowed(&res), "{path}");
    }
    for path in ["/oauth/token", "/oauth/register"] {
        let preflight = oauth_client()
            .request(reqwest::Method::OPTIONS, server.url(path))
            .header("origin", origin)
            .header("access-control-request-method", "POST")
            .send()
            .await
            .unwrap();
        assert!(allowed(&preflight), "{path}");
    }
    server.stop().await;
}
