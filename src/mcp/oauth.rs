//! OAuth 2.1 authorization server for `mt mcp`.

use std::sync::Arc;

use axum::{
    Form, Json, Router,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::Engine;
use chrono::{Duration, Utc};
use hmac::{Hmac, KeyInit, Mac};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tower_http::cors::{Any, CorsLayer};
use url::Url;

use crate::db::{self, Db, Op};
use crate::schema::{OAuthAccessToken, OAuthClient, OAuthCode, OAuthCodeUse, OAuthRefreshToken};

const AUTHORIZATION_CODE_TTL: Duration = Duration::seconds(60);
const ACCESS_TOKEN_TTL: Duration = Duration::hours(1);
const REFRESH_TOKEN_TTL: Duration = Duration::days(30);
const CSRF_TTL: Duration = Duration::minutes(15);

/// Shared OAuth state for the Axum router.
#[derive(Clone)]
pub struct OAuthState {
    db: Db,
    admin_password: Arc<[u8]>,
    public_url: Arc<Url>,
    csrf_key: Arc<[u8; 32]>,
}

impl OAuthState {
    pub fn new(db: Db, admin_password: &str, public_url: Url) -> Self {
        let mut csrf_key = [0u8; 32];
        rand::rng().fill_bytes(&mut csrf_key);
        Self {
            db,
            admin_password: Arc::from(admin_password.as_bytes().to_vec().into_boxed_slice()),
            public_url: Arc::new(public_url),
            csrf_key: Arc::new(csrf_key),
        }
    }

    /// Return the public issuer URL.
    pub fn issuer(&self) -> &Url {
        &self.public_url
    }
}

/// Build the OAuth discovery and authorization router.
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
    for uri in &req.redirect_uris {
        validate_redirect_uri(uri)?;
    }
    let client = OAuthClient {
        client_id: random_token(),
        client_name: req.client_name,
        redirect_uris: req.redirect_uris,
        created_at: Utc::now(),
    };
    s.db.write(&[Op::insert(&client)]).await?;

    tracing::info!(
        client_id = %client.client_id,
        client_name = client.client_name.as_deref().unwrap_or("<unnamed>"),
        "oauth client registered",
    );

    Ok(Json(RegisterResponse {
        client_id: client.client_id,
        client_name: client.client_name,
        redirect_uris: client.redirect_uris,
        grant_types: ["authorization_code", "refresh_token"],
        response_types: ["code"],
        token_endpoint_auth_method: "none",
    }))
}

// Reject unencrypted non-loopback HTTP redirect URIs to prevent authorization code interception.
fn validate_redirect_uri(uri: &str) -> Result<(), OAuthError> {
    let parsed = Url::parse(uri).map_err(|_| {
        OAuthError::invalid_request(format!("redirect_uri '{uri}' is not a valid URL"))
    })?;
    if parsed.fragment().is_some() {
        return Err(OAuthError::invalid_request(format!(
            "redirect_uri '{uri}' must not contain a fragment"
        )));
    }
    if parsed.scheme() == "http" {
        let loopback = match parsed.host() {
            Some(url::Host::Domain(d)) => d == "localhost",
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        };
        if !loopback {
            return Err(OAuthError::invalid_request(format!(
                "redirect_uri '{uri}' uses plain http on a non-loopback host"
            )));
        }
    }
    Ok(())
}

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
    validate_authorize_request(&s, &q).await?;
    let csrf = csrf_mint(&s.csrf_key, &q);
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
    if !csrf_verify(&s.csrf_key, &q, &f.csrf) {
        tracing::warn!(client_id = %client.client_id, "csrf token rejected on authorize POST");
        return Err(OAuthError::invalid_request("invalid or expired csrf token"));
    }
    if !constant_time_eq(s.admin_password.as_ref(), f.password.as_bytes()) {
        tracing::warn!(client_id = %client.client_id, "admin password rejected");
        let csrf = csrf_mint(&s.csrf_key, &q);
        let html = render_login_form(&q, &csrf, Some("Incorrect password."));
        return Ok((StatusCode::UNAUTHORIZED, Html(html)).into_response());
    }

    let code = OAuthCode {
        code: random_token(),
        client_id: client.client_id.clone(),
        redirect_uri: q.redirect_uri.clone(),
        scope: q.scope.clone(),
        code_challenge: q.code_challenge.clone(),
        code_challenge_method: "S256".into(),
        resource: q.resource.clone(),
        expires_at: Utc::now() + AUTHORIZATION_CODE_TTL,
        used_at: None,
    };
    s.db.write(&[Op::insert_expiring(&code, AUTHORIZATION_CODE_TTL)])
        .await?;

    tracing::info!(client_id = %client.client_id, "authorization code issued");

    let mut redirect = Url::parse(&q.redirect_uri)
        .map_err(|_| OAuthError::invalid_request("redirect_uri is not a valid URL"))?;
    {
        let mut pairs = redirect.query_pairs_mut();
        pairs.append_pair("code", &code.code);
        if let Some(state) = &q.state {
            pairs.append_pair("state", state);
        }
    }
    Ok(Redirect::to(redirect.as_str()).into_response())
}

async fn validate_authorize_request(
    s: &OAuthState,
    q: &AuthorizeQuery,
) -> Result<OAuthClient, OAuthError> {
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
    let client = load_client(&s.db, &q.client_id).await?;
    if !client.redirect_uris.iter().any(|u| u == &q.redirect_uri) {
        return Err(OAuthError::invalid_request(
            "redirect_uri does not match any registered redirect URI for the client",
        ));
    }
    Ok(client)
}

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

    // Consume the authorization code and issue tokens in a single transaction to prevent replay.
    let (access, refresh, stored_scope) = db::retry(|| async {
        let (txn, mut r) = s.db.begin(&[Op::get_row::<OAuthCode>(&[code])]).await?;
        let scope = match redeem(r.swap_remove(0).row()?, client_id, redirect_uri, verifier) {
            Ok(scope) => scope,
            Err(e) => {
                txn.abort().await;
                return Ok(Err(e));
            }
        };
        let (access, refresh) = (random_token(), random_token());
        let used = OAuthCodeUse {
            used_at: Utc::now(),
        };
        let mut ops = vec![Op::update::<OAuthCode>(&[code], &used)];
        ops.extend(token_ops(&access, &refresh, client_id, scope.as_deref()));
        txn.commit(&ops).await?;
        Ok(Ok((access, refresh, scope)))
    })
    .await??;
    tracing::info!(%client_id, "access + refresh tokens issued (auth_code)");

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

    // Delete the presented refresh token and insert a new pair in the same transaction.
    let (access, refresh, client_id, scope) = db::retry(|| async {
        let (txn, mut r) =
            s.db.begin(&[Op::get_row::<OAuthRefreshToken>(&[provided])])
                .await?;
        let stored = match check_refresh(r.swap_remove(0).row()?, req.client_id.as_deref()) {
            Ok(stored) => stored,
            Err(e) => {
                txn.abort().await;
                return Ok(Err(e));
            }
        };
        let scope = req.scope.clone().or(stored.scope);
        let (access, refresh) = (random_token(), random_token());
        let mut ops = vec![Op::delete_row::<OAuthRefreshToken>(&[provided])];
        ops.extend(token_ops(
            &access,
            &refresh,
            &stored.client_id,
            scope.as_deref(),
        ));
        txn.commit(&ops).await?;
        Ok(Ok((access, refresh, stored.client_id, scope)))
    })
    .await??;
    tracing::info!(%client_id, "tokens rotated (refresh_token)");

    Ok(Json(TokenResponse {
        access_token: access,
        token_type: "Bearer",
        expires_in: ACCESS_TOKEN_TTL.num_seconds(),
        refresh_token: refresh,
        scope,
    }))
}

fn redeem(
    row: Option<OAuthCode>,
    client_id: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<Option<String>, OAuthError> {
    let Some(row) = row else {
        return Err(OAuthError::invalid_grant());
    };
    if row.used_at.is_some() {
        tracing::warn!(%client_id, "auth code already used");
        return Err(OAuthError::invalid_grant());
    }
    if row.client_id != client_id {
        tracing::warn!(%client_id, stored_client_id = %row.client_id, "auth code client_id mismatch");
        return Err(OAuthError::invalid_grant());
    }
    if row.redirect_uri != redirect_uri {
        tracing::warn!(%client_id, "auth code redirect_uri mismatch");
        return Err(OAuthError::invalid_grant());
    }
    if row.expires_at < Utc::now() {
        tracing::warn!(%client_id, "auth code expired");
        return Err(OAuthError::invalid_grant());
    }
    if row.code_challenge != pkce_s256(verifier) {
        tracing::warn!(%client_id, "PKCE verifier did not match challenge");
        return Err(OAuthError::invalid_grant());
    }
    Ok(row.scope)
}

fn check_refresh(
    row: Option<OAuthRefreshToken>,
    client_id: Option<&str>,
) -> Result<OAuthRefreshToken, OAuthError> {
    let Some(row) = row else {
        return Err(OAuthError::invalid_grant());
    };
    if row.expires_at < Utc::now() {
        tracing::warn!(client_id = %row.client_id, "refresh token expired");
        return Err(OAuthError::invalid_grant());
    }
    if let Some(rc) = client_id
        && rc != row.client_id
    {
        tracing::warn!(client_id = %rc, stored_client_id = %row.client_id, "refresh token client_id mismatch");
        return Err(OAuthError::invalid_grant());
    }
    Ok(row)
}

fn token_ops(access: &str, refresh: &str, client_id: &str, scope: Option<&str>) -> [Op; 2] {
    let now = Utc::now();
    let access = OAuthAccessToken {
        token: access.into(),
        client_id: client_id.into(),
        scope: scope.map(Into::into),
        expires_at: now + ACCESS_TOKEN_TTL,
    };
    let refresh = OAuthRefreshToken {
        token: refresh.into(),
        client_id: client_id.into(),
        scope: scope.map(Into::into),
        expires_at: now + REFRESH_TOKEN_TTL,
    };
    [
        Op::insert_expiring(&access, ACCESS_TOKEN_TTL),
        Op::insert_expiring(&refresh, REFRESH_TOKEN_TTL),
    ]
}

/// Return the `client_id` for a non-expired access token, or `None` if unknown or expired.
pub async fn validate_access_token(db: &Db, token: &str) -> Result<Option<String>, crate::Error> {
    let stored: Option<OAuthAccessToken> = db
        .read_one(Op::get_row::<OAuthAccessToken>(&[token]))
        .await?
        .row()?;
    Ok(stored
        .filter(|t| t.expires_at >= Utc::now())
        .map(|t| t.client_id))
}

async fn load_client(db: &Db, client_id: &str) -> Result<OAuthClient, OAuthError> {
    db.read_one(Op::get_row::<OAuthClient>(&[client_id]))
        .await?
        .row()?
        .ok_or_else(|| OAuthError::invalid_request("unknown client_id"))
}

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

fn random_token() -> String {
    let mut buf = [0u8; 32];
    rand::rng().fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

fn pkce_s256(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.ct_eq(b).into()
}

fn csrf_mint(key: &[u8; 32], q: &AuthorizeQuery) -> String {
    let ts = Utc::now().timestamp();
    let mac_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(csrf_mac(key, ts, q));
    format!("{ts}.{mac_b64}")
}

fn csrf_verify(key: &[u8; 32], q: &AuthorizeQuery, token: &str) -> bool {
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
    let expected = csrf_mac(key, ts, q);
    provided.ct_eq(&expected).into()
}

// Length-prefix each field before hashing to prevent canonicalization ambiguity.
fn csrf_mac(key: &[u8; 32], ts: i64, q: &AuthorizeQuery) -> Vec<u8> {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(&ts.to_be_bytes());
    for field in [
        q.client_id.as_str(),
        q.redirect_uri.as_str(),
        q.code_challenge.as_str(),
    ] {
        let bytes = field.as_bytes();
        let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        mac.update(&len.to_be_bytes());
        mac.update(bytes);
    }
    mac.finalize().into_bytes().to_vec()
}

/// OAuth 2.1 error response variants.
#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error("invalid_request: {0}")]
    InvalidRequest(String),
    #[error("invalid_grant")]
    InvalidGrant,
    #[error("unsupported_response_type")]
    UnsupportedResponseType,
    #[error("unsupported_grant_type: {0}")]
    UnsupportedGrantType(String),
    #[error(transparent)]
    Internal(#[from] crate::Error),
}

impl OAuthError {
    fn invalid_request(msg: impl Into<String>) -> Self {
        Self::InvalidRequest(msg.into())
    }

    fn invalid_grant() -> Self {
        Self::InvalidGrant
    }

    fn unsupported_response_type() -> Self {
        Self::UnsupportedResponseType
    }

    fn unsupported_grant_type(grant: &str) -> Self {
        Self::UnsupportedGrantType(grant.to_string())
    }
}

impl IntoResponse for OAuthError {
    fn into_response(self) -> Response {
        let (status, code, description) = match &self {
            OAuthError::InvalidRequest(msg) => {
                (StatusCode::BAD_REQUEST, "invalid_request", msg.clone())
            }
            OAuthError::InvalidGrant => (
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "authorization grant is invalid, expired, revoked, or does not match".to_string(),
            ),
            OAuthError::UnsupportedResponseType => (
                StatusCode::BAD_REQUEST,
                "unsupported_response_type",
                "only `code` is supported".to_string(),
            ),
            OAuthError::UnsupportedGrantType(g) => (
                StatusCode::BAD_REQUEST,
                "unsupported_grant_type",
                format!("grant_type `{g}` is not supported"),
            ),
            OAuthError::Internal(e) => {
                tracing::error!(error = %e, "oauth: internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "server_error",
                    "internal server error".to_string(),
                )
            }
        };
        let mut headers = HeaderMap::new();
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        let body = serde_json::json!({
            "error": code,
            "error_description": description,
        });
        (status, headers, Json(body)).into_response()
    }
}
