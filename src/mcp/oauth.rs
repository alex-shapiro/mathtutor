//! OAuth 2.1 authorization server for `mt mcp`.
//!
//! Implements the minimum needed for Claude Desktop's "Add custom connector"
//! flow to register itself, log the operator in, and obtain an access token
//! it can use against `/mcp`. Everything here is single-user: every issued
//! token grants the same authority as the static `MT_API_KEY` bearer.
//!
//! Wire surface:
//!
//! - `GET  /.well-known/oauth-protected-resource` — RFC 9728
//! - `GET  /.well-known/oauth-authorization-server` — RFC 8414
//! - `POST /oauth/register` — RFC 7591 dynamic client registration (open)
//! - `GET  /oauth/authorize` — render login form
//! - `POST /oauth/authorize` — validate password, issue authorization code,
//!   redirect back to the client's `redirect_uri`
//! - `POST /oauth/token` — exchange code (with PKCE verifier) for tokens,
//!   or refresh
//!
//! Tokens are opaque 256-bit random strings; storage lives in the four
//! `oauth_*` SQL tables. PKCE S256 is mandatory.
//!
//! What this module deliberately leaves out: token revocation, consent
//! prompts (single-user implies trust), `scope` enforcement beyond
//! round-tripping it, and any persistent refresh-rotation linked list.
//! These are non-features for the single-user use case.

use std::sync::Arc;

use axum::{
    Form, Json, Router,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use libsql::{Connection, Database, params};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tower_http::cors::{Any, CorsLayer};
use url::Url;

use crate::db;

// ───────────────────────────── tunables ──────────────────────────────

const AUTHORIZATION_CODE_TTL: Duration = Duration::seconds(60);
const ACCESS_TOKEN_TTL: Duration = Duration::hours(1);
const REFRESH_TOKEN_TTL: Duration = Duration::days(30);

/// Maximum age accepted on the HMAC-signed CSRF token embedded in the
/// login form. Bounds how long a stale form can hang in the agent's web
/// view before submitting.
const CSRF_TTL: Duration = Duration::minutes(15);

// ───────────────────────────── state ─────────────────────────────────

/// Per-process OAuth state injected into the axum router. Cheap to clone:
/// every field is already an `Arc`.
#[derive(Clone)]
pub struct OAuthState {
    db: Arc<Database>,
    /// Constant-time-comparable copy of the admin password. Stored as
    /// bytes so the comparison can't short-circuit on length mismatch.
    admin_password: Arc<[u8]>,
    /// Public-facing URL used as both the AS issuer and the protected-
    /// resource identifier. Must match what the client sees, otherwise
    /// metadata discovery and `resource` parameter checks fail.
    public_url: Arc<Url>,
    /// HMAC key for the CSRF token embedded in the login form. Generated
    /// fresh per process — losing it on restart only invalidates open
    /// login pages, which is harmless.
    csrf_key: Arc<[u8; 32]>,
}

impl OAuthState {
    pub fn new(db: Arc<Database>, admin_password: &str, public_url: Url) -> Self {
        let mut csrf_key = [0u8; 32];
        rand::rng().fill_bytes(&mut csrf_key);
        Self {
            db,
            admin_password: Arc::from(admin_password.as_bytes().to_vec().into_boxed_slice()),
            public_url: Arc::new(public_url),
            csrf_key: Arc::new(csrf_key),
        }
    }

    /// Issuer URL used in `/.well-known/oauth-authorization-server`. Per
    /// RFC 8414 this is the public base; endpoints are absolute URLs
    /// rooted at it.
    pub fn issuer(&self) -> &Url {
        &self.public_url
    }

    async fn conn(&self) -> Result<Connection, OAuthError> {
        db::connect(&self.db)
            .await
            .map_err(|e| OAuthError::from_crate(&e))
    }
}

// ───────────────────────────── routes ────────────────────────────────

/// Public-facing well-known endpoints. Mounted at the server root, no
/// auth — clients hit these to discover the AS.
pub fn router(state: OAuthState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(authorization_server_metadata),
        )
        .route("/oauth/register", post(register))
        .route("/oauth/authorize", get(authorize_get).post(authorize_post))
        .route("/oauth/token", post(token))
        .layer(cors)
        .with_state(state)
}

// ───────────────────── /.well-known endpoints ────────────────────────

async fn protected_resource_metadata(State(s): State<OAuthState>) -> Json<serde_json::Value> {
    let issuer = s.issuer().as_str().trim_end_matches('/');
    Json(serde_json::json!({
        "resource": issuer,
        "authorization_servers": [issuer],
        "bearer_methods_supported": ["header"],
    }))
}

async fn authorization_server_metadata(State(s): State<OAuthState>) -> Json<serde_json::Value> {
    let issuer = s.issuer().as_str().trim_end_matches('/');
    Json(serde_json::json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/oauth/authorize"),
        "token_endpoint": format!("{issuer}/oauth/token"),
        "registration_endpoint": format!("{issuer}/oauth/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": ["mcp"],
    }))
}

// ─────────────────────────── /oauth/register ─────────────────────────

#[derive(Deserialize)]
struct RegisterRequest {
    #[serde(default)]
    client_name: Option<String>,
    redirect_uris: Vec<String>,
}

#[derive(Serialize)]
struct RegisterResponse {
    client_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_name: Option<String>,
    redirect_uris: Vec<String>,
    grant_types: [&'static str; 2],
    response_types: [&'static str; 1],
    token_endpoint_auth_method: &'static str,
}

async fn register(
    State(s): State<OAuthState>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<RegisterResponse>, OAuthError> {
    if req.redirect_uris.is_empty() {
        return Err(OAuthError::invalid_request(
            "redirect_uris must include at least one URI",
        ));
    }
    let conn = s.conn().await?;
    let client_id = random_token();
    let redirect_json = serde_json::to_string(&req.redirect_uris)
        .map_err(|e| OAuthError::server_error(format!("encode redirect_uris: {e}")))?;
    conn.execute(
        "INSERT INTO oauth_clients(client_id, client_name, redirect_uris, created_at) \
         VALUES (?, ?, ?, ?)",
        params![
            client_id.as_str(),
            req.client_name.as_deref(),
            redirect_json.as_str(),
            db::format_ts(Utc::now()),
        ],
    )
    .await
    .map_err(|e| OAuthError::from_db(&e))?;
    Ok(Json(RegisterResponse {
        client_id,
        client_name: req.client_name,
        redirect_uris: req.redirect_uris,
        grant_types: ["authorization_code", "refresh_token"],
        response_types: ["code"],
        token_endpoint_auth_method: "none",
    }))
}

// ─────────────────────────── /oauth/authorize ────────────────────────

#[derive(Deserialize)]
struct AuthorizeQuery {
    response_type: String,
    client_id: String,
    redirect_uri: String,
    #[serde(default)]
    state: Option<String>,
    code_challenge: String,
    code_challenge_method: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    resource: Option<String>,
}

async fn authorize_get(
    State(s): State<OAuthState>,
    Query(q): Query<AuthorizeQuery>,
) -> Result<Html<String>, OAuthError> {
    let client = validate_authorize_request(&s, &q).await?;
    let csrf = csrf_mint(&s.csrf_key, &client.client_id);
    Ok(Html(render_login_form(&q, &csrf, None)))
}

#[derive(Deserialize)]
struct AuthorizePost {
    response_type: String,
    client_id: String,
    redirect_uri: String,
    #[serde(default)]
    state: Option<String>,
    code_challenge: String,
    code_challenge_method: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    resource: Option<String>,
    csrf: String,
    password: String,
}

async fn authorize_post(
    State(s): State<OAuthState>,
    Form(f): Form<AuthorizePost>,
) -> Result<Response, OAuthError> {
    let q = AuthorizeQuery {
        response_type: f.response_type,
        client_id: f.client_id,
        redirect_uri: f.redirect_uri,
        state: f.state,
        code_challenge: f.code_challenge,
        code_challenge_method: f.code_challenge_method,
        scope: f.scope,
        resource: f.resource,
    };
    let client = validate_authorize_request(&s, &q).await?;
    if !csrf_verify(&s.csrf_key, &client.client_id, &f.csrf) {
        return Err(OAuthError::invalid_request("invalid or expired csrf token"));
    }
    if !constant_time_eq(s.admin_password.as_ref(), f.password.as_bytes()) {
        // Re-render the form with an inline error rather than redirecting:
        // the agent's web view follows the redirect_uri only on success.
        let csrf = csrf_mint(&s.csrf_key, &client.client_id);
        let html = render_login_form(&q, &csrf, Some("Incorrect password."));
        return Ok((StatusCode::UNAUTHORIZED, Html(html)).into_response());
    }

    let conn = s.conn().await?;
    let code = random_token();
    let now = Utc::now();
    conn.execute(
        "INSERT INTO oauth_authorization_codes(\
            code, client_id, redirect_uri, scope, \
            code_challenge, code_challenge_method, resource, expires_at\
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            code.as_str(),
            client.client_id.as_str(),
            q.redirect_uri.as_str(),
            q.scope.as_deref(),
            q.code_challenge.as_str(),
            "S256",
            q.resource.as_deref(),
            db::format_ts(now + AUTHORIZATION_CODE_TTL),
        ],
    )
    .await
    .map_err(|e| OAuthError::from_db(&e))?;

    let mut redirect = Url::parse(&q.redirect_uri)
        .map_err(|_| OAuthError::invalid_request("redirect_uri is not a valid URL"))?;
    {
        let mut pairs = redirect.query_pairs_mut();
        pairs.append_pair("code", &code);
        if let Some(state) = &q.state {
            pairs.append_pair("state", state);
        }
    }
    Ok(Redirect::to(redirect.as_str()).into_response())
}

/// Validate every field of an `/oauth/authorize` request (GET or POST).
/// On success returns the registered client so callers can use its name
/// for display purposes; on failure the caller maps the [`OAuthError`]
/// into the right response shape.
async fn validate_authorize_request(
    s: &OAuthState,
    q: &AuthorizeQuery,
) -> Result<RegisteredClient, OAuthError> {
    if q.response_type != "code" {
        return Err(OAuthError::unsupported_response_type());
    }
    if q.code_challenge_method != "S256" {
        return Err(OAuthError::invalid_request(
            "code_challenge_method must be S256",
        ));
    }
    if q.code_challenge.is_empty() {
        return Err(OAuthError::invalid_request("code_challenge is required"));
    }
    let conn = s.conn().await?;
    let client = load_client(&conn, &q.client_id).await?;
    if !client.redirect_uris.iter().any(|u| u == &q.redirect_uri) {
        return Err(OAuthError::invalid_request(
            "redirect_uri does not match any registered redirect URI for the client",
        ));
    }
    Ok(client)
}

// ─────────────────────────── /oauth/token ────────────────────────────

#[derive(Deserialize)]
struct TokenRequest {
    grant_type: String,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    redirect_uri: Option<String>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    code_verifier: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Serialize)]
struct TokenResponse {
    access_token: String,
    token_type: &'static str,
    expires_in: i64,
    refresh_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
}

async fn token(
    State(s): State<OAuthState>,
    Form(req): Form<TokenRequest>,
) -> Result<Json<TokenResponse>, OAuthError> {
    match req.grant_type.as_str() {
        "authorization_code" => token_authorization_code(&s, req).await,
        "refresh_token" => token_refresh(&s, req).await,
        other => Err(OAuthError::unsupported_grant_type(other)),
    }
}

async fn token_authorization_code(
    s: &OAuthState,
    req: TokenRequest,
) -> Result<Json<TokenResponse>, OAuthError> {
    let code = req
        .code
        .as_deref()
        .ok_or_else(|| OAuthError::invalid_request("code is required"))?;
    let verifier = req
        .code_verifier
        .as_deref()
        .ok_or_else(|| OAuthError::invalid_request("code_verifier is required"))?;
    let redirect_uri = req
        .redirect_uri
        .as_deref()
        .ok_or_else(|| OAuthError::invalid_request("redirect_uri is required"))?;
    let client_id = req
        .client_id
        .as_deref()
        .ok_or_else(|| OAuthError::invalid_request("client_id is required"))?;

    let conn = s.conn().await?;
    let tx = conn
        .transaction()
        .await
        .map_err(|e| OAuthError::from_db(&e))?;

    let mut rows = tx
        .query(
            "SELECT client_id, redirect_uri, scope, code_challenge, expires_at, used_at \
             FROM oauth_authorization_codes WHERE code = ?",
            params![code],
        )
        .await
        .map_err(|e| OAuthError::from_db(&e))?;
    let row = rows
        .next()
        .await
        .map_err(|e| OAuthError::from_db(&e))?
        .ok_or_else(OAuthError::invalid_grant)?;

    let stored_client_id: String = row.get(0).map_err(|e| OAuthError::from_db(&e))?;
    let stored_redirect: String = row.get(1).map_err(|e| OAuthError::from_db(&e))?;
    let stored_scope: Option<String> = row.get(2).map_err(|e| OAuthError::from_db(&e))?;
    let stored_challenge: String = row.get(3).map_err(|e| OAuthError::from_db(&e))?;
    let expires_at: String = row.get(4).map_err(|e| OAuthError::from_db(&e))?;
    let used_at: Option<String> = row.get(5).map_err(|e| OAuthError::from_db(&e))?;
    drop(rows);

    if used_at.is_some() {
        return Err(OAuthError::invalid_grant());
    }
    if stored_client_id != client_id {
        return Err(OAuthError::invalid_grant());
    }
    if stored_redirect != redirect_uri {
        return Err(OAuthError::invalid_grant());
    }
    if db::parse_ts(&expires_at).map_err(|e| OAuthError::from_crate(&e))? < Utc::now() {
        return Err(OAuthError::invalid_grant());
    }
    if pkce_s256(verifier) != stored_challenge {
        return Err(OAuthError::invalid_grant());
    }

    // Burn the code and mint the tokens in the same tx; otherwise a
    // racing duplicate `/token` call could replay the same code.
    tx.execute(
        "UPDATE oauth_authorization_codes SET used_at = ? WHERE code = ?",
        params![db::format_ts(Utc::now()), code],
    )
    .await
    .map_err(|e| OAuthError::from_db(&e))?;

    let access = random_token();
    let refresh = random_token();
    let access_exp = Utc::now() + ACCESS_TOKEN_TTL;
    let refresh_exp = Utc::now() + REFRESH_TOKEN_TTL;
    insert_access_token(&tx, &access, client_id, stored_scope.as_deref(), access_exp).await?;
    insert_refresh_token(
        &tx,
        &refresh,
        client_id,
        stored_scope.as_deref(),
        refresh_exp,
    )
    .await?;
    tx.commit().await.map_err(|e| OAuthError::from_db(&e))?;

    Ok(Json(TokenResponse {
        access_token: access,
        token_type: "Bearer",
        expires_in: ACCESS_TOKEN_TTL.num_seconds(),
        refresh_token: refresh,
        scope: stored_scope,
    }))
}

async fn token_refresh(
    s: &OAuthState,
    req: TokenRequest,
) -> Result<Json<TokenResponse>, OAuthError> {
    let provided = req
        .refresh_token
        .as_deref()
        .ok_or_else(|| OAuthError::invalid_request("refresh_token is required"))?;

    let conn = s.conn().await?;
    let tx = conn
        .transaction()
        .await
        .map_err(|e| OAuthError::from_db(&e))?;

    let mut rows = tx
        .query(
            "SELECT client_id, scope, expires_at FROM oauth_refresh_tokens WHERE token = ?",
            params![provided],
        )
        .await
        .map_err(|e| OAuthError::from_db(&e))?;
    let row = rows
        .next()
        .await
        .map_err(|e| OAuthError::from_db(&e))?
        .ok_or_else(OAuthError::invalid_grant)?;

    let client_id: String = row.get(0).map_err(|e| OAuthError::from_db(&e))?;
    let stored_scope: Option<String> = row.get(1).map_err(|e| OAuthError::from_db(&e))?;
    let expires_at: String = row.get(2).map_err(|e| OAuthError::from_db(&e))?;
    drop(rows);

    if db::parse_ts(&expires_at).map_err(|e| OAuthError::from_crate(&e))? < Utc::now() {
        return Err(OAuthError::invalid_grant());
    }
    // If the caller pinned `client_id` in the refresh request, enforce it.
    if let Some(rc) = req.client_id.as_deref()
        && rc != client_id
    {
        return Err(OAuthError::invalid_grant());
    }

    // Rotate: invalidate the presented refresh token and mint a fresh
    // pair so an intercepted refresh token can only be used once.
    tx.execute(
        "DELETE FROM oauth_refresh_tokens WHERE token = ?",
        params![provided],
    )
    .await
    .map_err(|e| OAuthError::from_db(&e))?;

    let access = random_token();
    let refresh = random_token();
    let scope = req.scope.or(stored_scope.clone());
    let access_exp = Utc::now() + ACCESS_TOKEN_TTL;
    let refresh_exp = Utc::now() + REFRESH_TOKEN_TTL;
    insert_access_token(&tx, &access, &client_id, scope.as_deref(), access_exp).await?;
    insert_refresh_token(&tx, &refresh, &client_id, scope.as_deref(), refresh_exp).await?;
    tx.commit().await.map_err(|e| OAuthError::from_db(&e))?;

    Ok(Json(TokenResponse {
        access_token: access,
        token_type: "Bearer",
        expires_in: ACCESS_TOKEN_TTL.num_seconds(),
        refresh_token: refresh,
        scope,
    }))
}

async fn insert_access_token(
    conn: &Connection,
    token: &str,
    client_id: &str,
    scope: Option<&str>,
    expires_at: DateTime<Utc>,
) -> Result<(), OAuthError> {
    conn.execute(
        "INSERT INTO oauth_access_tokens(token, client_id, scope, expires_at) \
         VALUES (?, ?, ?, ?)",
        params![token, client_id, scope, db::format_ts(expires_at)],
    )
    .await
    .map_err(|e| OAuthError::from_db(&e))?;
    Ok(())
}

async fn insert_refresh_token(
    conn: &Connection,
    token: &str,
    client_id: &str,
    scope: Option<&str>,
    expires_at: DateTime<Utc>,
) -> Result<(), OAuthError> {
    conn.execute(
        "INSERT INTO oauth_refresh_tokens(token, client_id, scope, expires_at) \
         VALUES (?, ?, ?, ?)",
        params![token, client_id, scope, db::format_ts(expires_at)],
    )
    .await
    .map_err(|e| OAuthError::from_db(&e))?;
    Ok(())
}

// ─────────────────── access-token validation (for /mcp) ──────────────

/// Look up an access token and confirm it hasn't expired. Returns `Ok(Some)`
/// for a valid bearer, `Ok(None)` for unknown/expired (so the caller can
/// fall through to other auth modes before issuing a 401).
pub async fn validate_access_token(
    db: &Database,
    token: &str,
) -> Result<Option<String>, crate::Error> {
    let conn = db::connect(db).await?;
    let mut rows = conn
        .query(
            "SELECT client_id, expires_at FROM oauth_access_tokens WHERE token = ?",
            params![token],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let client_id: String = row.get(0)?;
    let expires_at: String = row.get(1)?;
    if db::parse_ts(&expires_at)? < Utc::now() {
        return Ok(None);
    }
    Ok(Some(client_id))
}

// ─────────────────────────── SQL helpers ─────────────────────────────

struct RegisteredClient {
    client_id: String,
    redirect_uris: Vec<String>,
}

async fn load_client(conn: &Connection, client_id: &str) -> Result<RegisteredClient, OAuthError> {
    let mut rows = conn
        .query(
            "SELECT client_id, redirect_uris FROM oauth_clients WHERE client_id = ?",
            params![client_id],
        )
        .await
        .map_err(|e| OAuthError::from_db(&e))?;
    let row = rows
        .next()
        .await
        .map_err(|e| OAuthError::from_db(&e))?
        .ok_or_else(|| OAuthError::invalid_request("unknown client_id"))?;
    let id: String = row.get(0).map_err(|e| OAuthError::from_db(&e))?;
    let uris_json: String = row.get(1).map_err(|e| OAuthError::from_db(&e))?;
    let redirect_uris: Vec<String> = serde_json::from_str(&uris_json)
        .map_err(|e| OAuthError::server_error(format!("decode redirect_uris: {e}")))?;
    Ok(RegisteredClient {
        client_id: id,
        redirect_uris,
    })
}

// ─────────────────────────── login form ──────────────────────────────

fn render_login_form(q: &AuthorizeQuery, csrf: &str, error: Option<&str>) -> String {
    fn esc(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }
    let scope = q.scope.as_deref().unwrap_or("");
    let resource = q.resource.as_deref().unwrap_or("");
    let state = q.state.as_deref().unwrap_or("");
    let error_html = error.map_or(String::new(), |msg| {
        format!(r#"<p style="color:#b00020">{}</p>"#, esc(msg))
    });
    format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><title>mt mcp — sign in</title>
<style>
body {{ font: 14px/1.5 system-ui, sans-serif; max-width: 22rem; margin: 4rem auto; padding: 0 1rem; }}
h1 {{ font-size: 1.1rem; margin-bottom: 0.5rem; }}
p {{ color: #555; }}
input[type=password] {{ width: 100%; padding: 0.5rem; font-size: 1rem; box-sizing: border-box; }}
button {{ margin-top: 1rem; padding: 0.5rem 1rem; font-size: 1rem; }}
</style></head>
<body>
<h1>Sign in to mt mcp</h1>
<p>Authorizing client <code>{client_id}</code>. Enter the admin password to grant access.</p>
{error_html}
<form method="post" action="/oauth/authorize">
<input type="hidden" name="response_type" value="{response_type}">
<input type="hidden" name="client_id" value="{client_id}">
<input type="hidden" name="redirect_uri" value="{redirect_uri}">
<input type="hidden" name="code_challenge" value="{code_challenge}">
<input type="hidden" name="code_challenge_method" value="{ccm}">
<input type="hidden" name="state" value="{state}">
<input type="hidden" name="scope" value="{scope}">
<input type="hidden" name="resource" value="{resource}">
<input type="hidden" name="csrf" value="{csrf}">
<label for="password">Password</label>
<input id="password" type="password" name="password" autocomplete="current-password" autofocus required>
<button type="submit">Sign in</button>
</form>
</body></html>"#,
        client_id = esc(&q.client_id),
        response_type = esc(&q.response_type),
        redirect_uri = esc(&q.redirect_uri),
        code_challenge = esc(&q.code_challenge),
        ccm = esc(&q.code_challenge_method),
        state = esc(state),
        scope = esc(scope),
        resource = esc(resource),
        csrf = esc(csrf),
    )
}

// ──────────────────────────── crypto bits ────────────────────────────

fn random_token() -> String {
    let mut buf = [0u8; 32];
    rand::rng().fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

/// RFC 7636 §4.2: `BASE64URL(SHA256(verifier))`, no padding.
fn pkce_s256(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.ct_eq(b).into()
}

// ── CSRF: HMAC-signed `<timestamp>.<mac>` value embedded in the form ──
//
// `client_id` is included in the MAC so a token minted for one
// authorization request can't be replayed against another client. We
// don't bother encoding `redirect_uri` etc. because the post handler
// re-validates those against the persisted client record anyway.

fn csrf_mint(key: &[u8; 32], client_id: &str) -> String {
    let ts = Utc::now().timestamp();
    let mac_b64 =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(csrf_mac(key, ts, client_id));
    format!("{ts}.{mac_b64}")
}

fn csrf_verify(key: &[u8; 32], client_id: &str, token: &str) -> bool {
    let Some((ts_str, mac_b64)) = token.split_once('.') else {
        return false;
    };
    let Ok(ts) = ts_str.parse::<i64>() else {
        return false;
    };
    let age = Utc::now().timestamp() - ts;
    if age < 0 || age > CSRF_TTL.num_seconds() {
        return false;
    }
    let Ok(provided) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(mac_b64) else {
        return false;
    };
    let expected = csrf_mac(key, ts, client_id);
    provided.ct_eq(&expected).into()
}

fn csrf_mac(key: &[u8; 32], ts: i64, client_id: &str) -> Vec<u8> {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(&ts.to_be_bytes());
    mac.update(b".");
    mac.update(client_id.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

// ──────────────────────────── errors ─────────────────────────────────

/// OAuth error codes per RFC 6749 §4.1.2.1 / §5.2. Each variant becomes
/// a JSON body `{ "error": "<code>", "error_description": "..." }` plus
/// an appropriate HTTP status.
#[derive(Debug)]
pub struct OAuthError {
    status: StatusCode,
    code: &'static str,
    description: String,
}

impl OAuthError {
    fn invalid_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_request",
            description: msg.into(),
        }
    }

    fn invalid_grant() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_grant",
            description: "authorization grant is invalid, expired, revoked, or does not match"
                .into(),
        }
    }

    fn unsupported_response_type() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "unsupported_response_type",
            description: "only `code` is supported".into(),
        }
    }

    fn unsupported_grant_type(grant: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "unsupported_grant_type",
            description: format!("grant_type `{grant}` is not supported"),
        }
    }

    fn server_error(msg: String) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "server_error",
            description: msg,
        }
    }

    fn from_db(e: &libsql::Error) -> Self {
        Self::server_error(format!("database: {e}"))
    }

    fn from_crate(e: &crate::Error) -> Self {
        Self::server_error(format!("{e}"))
    }
}

impl IntoResponse for OAuthError {
    fn into_response(self) -> Response {
        let mut headers = HeaderMap::new();
        // OAuth spec: error responses must not be cached.
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        let body = serde_json::json!({
            "error": self.code,
            "error_description": self.description,
        });
        (self.status, headers, Json(body)).into_response()
    }
}
