//! OAuth 2.1 authorization server for `mt mcp`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    Form, Json, Router,
    extract::{ConnectInfo, Query, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, KeyInit, Mac};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tower_http::cors::{Any, CorsLayer};
use url::Url;

use super::throttle::{self, AddrLimits};
use crate::db::{Db, Op, Txn, run_txn};
use crate::schema::{OAuthAccessToken, OAuthClient, OAuthCode, OAuthKey, OAuthRefreshToken};

const AUTHORIZATION_CODE_TTL: Duration = Duration::seconds(60);
const ACCESS_TOKEN_TTL: Duration = Duration::hours(1);
const REFRESH_TOKEN_TTL: Duration = Duration::days(30);
const CSRF_TTL: Duration = Duration::minutes(15);

/// Time after registration at which the server deletes a client without a token grant.
const UNUSED_CLIENT_TTL: Duration = Duration::days(1);

/// Scope that gives access to the MCP tools, and the only scope that this server grants.
const MCP_SCOPE: &str = "mcp";

/// Name of the stored key that signs the CSRF tokens of the sign-in form.
const CSRF_KEY_NAME: &str = "csrf";

type HmacSha256 = Hmac<Sha256>;

/// Shared OAuth state for the Axum router.
#[derive(Clone)]
pub struct OAuthState {
    db: Db,
    admin_password: Arc<[u8]>,
    public_url: Arc<Url>,
    csrf_key: Arc<tokio::sync::OnceCell<CsrfKey>>,
    limits: AddrLimits,
}

impl OAuthState {
    /// Create the state of a server whose sign-in form signs its CSRF tokens with the key that `db` stores.
    ///
    /// Every server on the same database accepts the forms of the others.
    pub(crate) fn new(db: Db, admin_password: &str, public_url: Url, limits: AddrLimits) -> Self {
        Self {
            db,
            admin_password: Arc::from(admin_password.as_bytes().to_vec().into_boxed_slice()),
            public_url: Arc::new(public_url),
            csrf_key: Arc::default(),
            limits,
        }
    }

    /// Return the key that signs the CSRF tokens of the sign-in form, and store a new random key if the database holds none.
    async fn csrf_key(&self) -> Result<&CsrfKey, OAuthError> {
        self.csrf_key
            .get_or_try_init(|| async {
                let key = run_txn!(&self.db, |txn| load_or_create_key(&mut txn, CSRF_KEY_NAME)
                    .await)?;
                Ok::<_, OAuthError>(CsrfKey(key.into_bytes()))
            })
            .await
    }

    /// Return the public issuer URL.
    pub fn issuer(&self) -> &Url {
        &self.public_url
    }
}

/// Build the OAuth discovery and authorization router.
///
/// Browser-based MCP clients read the metadata, register, and exchange tokens with `fetch`, so only those routes allow other origins. The sign-in form at `/oauth/authorize` is a page that the browser opens, and other origins cannot read it.
pub fn router(state: OAuthState) -> Router {
    let limit = middleware::from_fn_with_state(state.clone(), limit_requests);
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST])
        .allow_headers(Any);

    let token = Router::new()
        .route("/oauth/token", post(token))
        .route_layer(limit.clone());
    let cross_origin = Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(authorization_server_metadata),
        )
        .route("/oauth/register", post(register))
        .merge(token)
        .layer(cors);
    Router::new()
        .route("/oauth/authorize", get(authorize_get).post(authorize_post))
        .route_layer(limit)
        .route_layer(middleware::map_response(deny_framing))
        .merge(cross_origin)
        .with_state(state)
}

/// Forbid every site to show `resp` in a frame, so that no site can overlay the sign-in form and trick the owner into a sign-in.
async fn deny_framing(mut resp: Response) -> Response {
    let headers = resp.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("frame-ancestors 'none'"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    resp
}

/// Refuse a request from a client address over its request limit before the route reads the database.
async fn limit_requests(
    State(s): State<OAuthState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let addr = s.limits.client_addr(peer, req.headers());
    match s.limits.try_request(addr) {
        Some(wait) => {
            tracing::warn!(%addr, path = %req.uri().path(), "OAuth request limit reached");
            OAuthError::TooManyRequests(wait).into_response()
        }
        None => next.run(req).await,
    }
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
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<RegisterResponse>, OAuthError> {
    let addr = s.limits.client_addr(peer, &headers);
    if let Some(wait) = s.limits.try_register(addr).await? {
        tracing::warn!(%addr, "client registration limit reached");
        return Err(OAuthError::TooManyRequests(wait));
    }
    if req.redirect_uris.is_empty() {
        return Err(OAuthError::invalid_request(
            "redirect_uris must not be empty",
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
    s.db.insert_oauth(&client, std_ttl(UNUSED_CLIENT_TTL))
        .await?;

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
    let client = validate_authorize_request(&s, &q).await?;
    let csrf = csrf_mint(s.csrf_key().await?, &q);
    Ok(Html(render_login_form(&client, &q, &csrf, None)))
}

#[derive(Deserialize)]
struct AuthorizePost {
    #[serde(flatten)]
    query: AuthorizeQuery,
    csrf: String,
    password: String,
}

async fn authorize_post(
    State(s): State<OAuthState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(f): Form<AuthorizePost>,
) -> Result<Response, OAuthError> {
    let q = f.query;
    let client = validate_authorize_request(&s, &q).await?;
    let csrf_key = s.csrf_key().await?;
    if !csrf_verify(csrf_key, &q, &f.csrf) {
        tracing::warn!(client_id = %client.client_id, "csrf token rejected on authorize POST");
        let message = "This sign-in form expired. Enter the password again.";
        let html = render_login_form(&client, &q, &csrf_mint(csrf_key, &q), Some(message));
        return Ok((StatusCode::BAD_REQUEST, Html(html)).into_response());
    }
    let addr = s.limits.client_addr(peer, &headers);
    if let Some(wait) = s.limits.try_password(addr).await? {
        tracing::warn!(%addr, client_id = %client.client_id, "sign-in attempt delayed");
        let secs = throttle::retry_after_secs(wait);
        let unit = if secs == 1 { "second" } else { "seconds" };
        let message = format!("Too many incorrect passwords. Try again in {secs} {unit}.");
        let html = render_login_form(&client, &q, &csrf_mint(csrf_key, &q), Some(&message));
        return Ok((
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, secs.to_string())],
            Html(html),
        )
            .into_response());
    }
    if !constant_time_eq(s.admin_password.as_ref(), f.password.as_bytes()) {
        tracing::warn!(%addr, client_id = %client.client_id, "admin password rejected");
        let csrf = csrf_mint(csrf_key, &q);
        let html = render_login_form(&client, &q, &csrf, Some("Incorrect password."));
        return Ok((StatusCode::UNAUTHORIZED, Html(html)).into_response());
    }
    s.limits.record_password_success(addr).await?;

    let secret = random_token();
    let code = OAuthCode {
        code_hash: secret_hash(&secret),
        client_id: client.client_id.clone(),
        redirect_uri: q.redirect_uri.clone(),
        code_challenge: q.code_challenge.clone(),
        code_challenge_method: "S256".into(),
        resource: q.resource.clone(),
        expires_at: Utc::now() + AUTHORIZATION_CODE_TTL,
    };
    s.db.insert_oauth(&code, std_ttl(AUTHORIZATION_CODE_TTL))
        .await?;

    tracing::info!(client_id = %client.client_id, "authorization code issued");

    let mut redirect = Url::parse(&q.redirect_uri)
        .map_err(|_| OAuthError::invalid_request("redirect_uri is not a valid URL"))?;
    {
        let mut pairs = redirect.query_pairs_mut();
        pairs.append_pair("code", &secret);
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
        return Err(OAuthError::UnsupportedResponseType);
    }
    if q.code_challenge_method != "S256" {
        return Err(OAuthError::invalid_request(
            "code_challenge_method must be S256",
        ));
    }
    if q.code_challenge.is_empty() {
        return Err(OAuthError::invalid_request("code_challenge is required"));
    }
    check_scope(q.scope.as_deref())?;
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
    scope: &'static str,
}

async fn token(
    State(s): State<OAuthState>,
    Form(req): Form<TokenRequest>,
) -> Result<Json<TokenResponse>, OAuthError> {
    match req.grant_type.as_str() {
        "authorization_code" => token_authorization_code(&s, req).await,
        "refresh_token" => token_refresh(&s, req).await,
        other => Err(OAuthError::UnsupportedGrantType(other.to_string())),
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

    // Consume the authorization code and issue tokens atomically to prevent replay.
    let tokens = run_txn!(&s.db, |txn| {
        exchange_code(&mut txn, code, client_id, redirect_uri, verifier).await
    })?;
    tracing::info!(%client_id, "access + refresh tokens issued (auth_code)");
    Ok(Json(tokens.into()))
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
    let tokens = run_txn!(&s.db, |txn| rotate_refresh(&mut txn, provided, &req).await)?;
    tracing::info!(client_id = %tokens.client_id, "tokens rotated (refresh_token)");
    Ok(Json(tokens.into()))
}

/// Access and refresh tokens that a grant issues to a client.
struct IssuedTokens {
    access: String,
    refresh: String,
    client_id: String,
}

impl IssuedTokens {
    /// Mint new tokens for `client_id`.
    fn new(client_id: &str) -> Self {
        Self {
            access: random_token(),
            refresh: random_token(),
            client_id: client_id.to_string(),
        }
    }

    /// Queue the inserts that store these tokens in `txn`.
    fn insert(&self, txn: &mut Txn) {
        let now = Utc::now();
        let access = OAuthAccessToken {
            token_hash: secret_hash(&self.access),
            client_id: self.client_id.clone(),
            expires_at: now + ACCESS_TOKEN_TTL,
        };
        let refresh = OAuthRefreshToken {
            token_hash: secret_hash(&self.refresh),
            client_id: self.client_id.clone(),
            expires_at: now + REFRESH_TOKEN_TTL,
        };
        txn.insert_oauth(&access, std_ttl(ACCESS_TOKEN_TTL));
        txn.insert_oauth(&refresh, std_ttl(REFRESH_TOKEN_TTL));
    }
}

impl From<IssuedTokens> for TokenResponse {
    fn from(t: IssuedTokens) -> Self {
        Self {
            access_token: t.access,
            token_type: "Bearer",
            expires_in: ACCESS_TOKEN_TTL.num_seconds(),
            refresh_token: t.refresh,
            scope: MCP_SCOPE,
        }
    }
}

/// Delete authorization `code` and queue new tokens.
async fn exchange_code(
    txn: &mut Txn,
    code: &str,
    client_id: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<IssuedTokens, OAuthError> {
    let code_hash = secret_hash(code);
    let row = txn.get_oauth::<OAuthCode>(&code_hash).await?;
    redeem(row, client_id, redirect_uri, verifier)?;
    let tokens = IssuedTokens::new(client_id);
    txn.delete_oauth::<OAuthCode>(&code_hash);
    keep_client(txn, client_id).await?;
    tokens.insert(txn);
    Ok(tokens)
}

/// Replace refresh token `provided` with new tokens.
///
/// A request from an unknown client fails with `invalid_client` also if its refresh token expired, so that the client registers again.
async fn rotate_refresh(
    txn: &mut Txn,
    provided: &str,
    req: &TokenRequest,
) -> Result<IssuedTokens, OAuthError> {
    let token_hash = secret_hash(provided);
    let row = txn.get_oauth::<OAuthRefreshToken>(&token_hash).await?;
    let client_id = req
        .client_id
        .as_deref()
        .or(row.as_ref().map(|r| r.client_id.as_str()));
    if let Some(client_id) = client_id {
        keep_client(txn, client_id).await?;
    }
    let stored = check_refresh(row, req.client_id.as_deref())?;
    check_scope(req.scope.as_deref())?;
    let tokens = IssuedTokens::new(&stored.client_id);
    txn.delete_oauth::<OAuthRefreshToken>(&token_hash);
    tokens.insert(txn);
    Ok(tokens)
}

/// Keep the registration of `client_id` without an expiry after a token grant, or fail with `invalid_client` if the client does not exist.
async fn keep_client(txn: &mut Txn, client_id: &str) -> Result<(), OAuthError> {
    let Some(client) = txn.get_oauth::<OAuthClient>(client_id).await? else {
        tracing::warn!(%client_id, "token grant for an unknown client");
        return Err(OAuthError::InvalidClient);
    };
    txn.delete_oauth::<OAuthClient>(client_id);
    txn.insert_lasting_oauth(&client);
    Ok(())
}

/// Accept a requested scope that is absent or names only [`MCP_SCOPE`], the one scope that this server grants.
///
/// RFC 6749 section 3.3 lets the server apply a default scope if the client omits one.
fn check_scope(requested: Option<&str>) -> Result<(), OAuthError> {
    let mut requested = requested.into_iter().flat_map(str::split_whitespace);
    if requested.all(|s| s == MCP_SCOPE) {
        Ok(())
    } else {
        Err(OAuthError::InvalidScope)
    }
}

fn std_ttl(ttl: Duration) -> std::time::Duration {
    ttl.to_std().expect("token lifetimes are positive")
}

fn redeem(
    row: Option<OAuthCode>,
    client_id: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<(), OAuthError> {
    let Some(row) = row else {
        tracing::warn!(%client_id, "auth code unknown or already used");
        return Err(OAuthError::InvalidGrant);
    };
    if row.client_id != client_id {
        tracing::warn!(%client_id, stored_client_id = %row.client_id, "auth code client_id mismatch");
        return Err(OAuthError::InvalidGrant);
    }
    if row.redirect_uri != redirect_uri {
        tracing::warn!(%client_id, "auth code redirect_uri mismatch");
        return Err(OAuthError::InvalidGrant);
    }
    if row.expires_at < Utc::now() {
        tracing::warn!(%client_id, "auth code expired");
        return Err(OAuthError::InvalidGrant);
    }
    if row.code_challenge != pkce_s256(verifier) {
        tracing::warn!(%client_id, "PKCE verifier did not match challenge");
        return Err(OAuthError::InvalidGrant);
    }
    Ok(())
}

fn check_refresh(
    row: Option<OAuthRefreshToken>,
    client_id: Option<&str>,
) -> Result<OAuthRefreshToken, OAuthError> {
    let Some(row) = row else {
        return Err(OAuthError::InvalidGrant);
    };
    if row.expires_at < Utc::now() {
        tracing::warn!(client_id = %row.client_id, "refresh token expired");
        return Err(OAuthError::InvalidGrant);
    }
    if let Some(rc) = client_id
        && rc != row.client_id
    {
        tracing::warn!(client_id = %rc, stored_client_id = %row.client_id, "refresh token client_id mismatch");
        return Err(OAuthError::InvalidGrant);
    }
    Ok(row)
}

/// Number of valid access tokens that the server keeps in memory.
const MAX_CACHED_ACCESS_TOKENS: usize = 1_000;

/// Access tokens that give access to the MCP tools, with a memory cache of the valid tokens.
///
/// The server has no revocation endpoint, so a cached token stays valid exactly as long as its row.
#[derive(Clone)]
pub struct AccessTokens {
    db: Db,
    /// Expiry time of each valid access token, keyed by the hash of the token.
    valid: Arc<std::sync::Mutex<HashMap<String, DateTime<Utc>>>>,
}

impl AccessTokens {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            valid: Arc::default(),
        }
    }

    /// Return `true` if the memory of this server holds `token` as a valid access token.
    pub fn is_cached(&self, token: &str) -> bool {
        self.cached(&secret_hash(token), Utc::now())
    }

    /// Return `true` if `token` is a known, unexpired access token.
    pub async fn is_valid(&self, token: &str) -> Result<bool, crate::Error> {
        let hash = secret_hash(token);
        let now = Utc::now();
        if self.cached(&hash, now) {
            return Ok(true);
        }
        let stored: Option<OAuthAccessToken> = self
            .db
            .read_one(Op::get_row::<OAuthAccessToken>(&hash))
            .await?
            .row()?;
        let Some(expires_at) = stored.filter(|t| t.expires_at >= now).map(|t| t.expires_at) else {
            return Ok(false);
        };
        self.remember(hash, expires_at, now);
        Ok(true)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, DateTime<Utc>>> {
        self.valid
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Return `true` if the cache holds token hash `hash` unexpired at `now`, and forget the hash if it expired.
    fn cached(&self, hash: &str, now: DateTime<Utc>) -> bool {
        let mut valid = self.lock();
        match valid.get(hash) {
            Some(expires_at) if *expires_at >= now => true,
            Some(_) => {
                valid.remove(hash);
                false
            }
            None => false,
        }
    }

    /// Cache token hash `hash` until `expires_at`, and make room by evicting expired tokens, then the token that expires first.
    fn remember(&self, hash: String, expires_at: DateTime<Utc>, now: DateTime<Utc>) {
        let mut valid = self.lock();
        if valid.len() >= MAX_CACHED_ACCESS_TOKENS {
            valid.retain(|_, e| *e >= now);
        }
        if valid.len() >= MAX_CACHED_ACCESS_TOKENS
            && let Some(first) = valid
                .iter()
                .min_by_key(|(_, e)| **e)
                .map(|(h, _)| h.clone())
        {
            valid.remove(&first);
        }
        valid.insert(hash, expires_at);
    }
}

async fn load_client(db: &Db, client_id: &str) -> Result<OAuthClient, OAuthError> {
    db.read_one(Op::get_row::<OAuthClient>(client_id))
        .await?
        .row()?
        .ok_or_else(|| OAuthError::invalid_request("unknown client_id"))
}

/// Render the sign-in form that grants `client` access, with the host that receives the code.
fn render_login_form(
    client: &OAuthClient,
    q: &AuthorizeQuery,
    csrf: &str,
    error: Option<&str>,
) -> String {
    fn esc(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }
    let optional_inputs: String = [
        ("state", &q.state),
        ("scope", &q.scope),
        ("resource", &q.resource),
    ]
    .into_iter()
    .filter_map(|(name, value)| {
        let value = esc(value.as_deref()?);
        Some(format!(
            "<input type=\"hidden\" name=\"{name}\" value=\"{value}\">\n"
        ))
    })
    .collect();
    let error_html = error.map_or(String::new(), |msg| {
        format!(r#"<p style="color:#b00020">{}</p>"#, esc(msg))
    });
    let client_name = client.client_name.as_deref().unwrap_or("An unnamed client");
    let redirect_host = redirect_host(&q.redirect_uri);
    format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><title>mt mcp — sign in</title>
<style>
body {{ font: 14px/1.5 system-ui, sans-serif; max-width: 22rem; margin: 4rem auto; padding: 0 1rem; }}
h1 {{ font-size: 1.1rem; margin-bottom: 0.5rem; }}
p {{ color: #555; }}
.grant {{ color: #000; font-size: 1rem; }}
input[type=password] {{ width: 100%; padding: 0.5rem; font-size: 1rem; box-sizing: border-box; }}
button {{ margin-top: 1rem; padding: 0.5rem 1rem; font-size: 1rem; }}
</style></head>
<body>
<h1>Sign in to mt mcp</h1>
<p class="grant"><strong>{client_name}</strong> asks for access to your Math Tutor data. After you sign in, your browser goes to <strong>{redirect_host}</strong>.</p>
<p>Sign in only if you started this connection, and only if you trust {redirect_host}. Client ID: <code>{client_id}</code>.</p>
{error_html}
<form method="post" action="/oauth/authorize">
<input type="hidden" name="response_type" value="{response_type}">
<input type="hidden" name="client_id" value="{client_id}">
<input type="hidden" name="redirect_uri" value="{redirect_uri}">
<input type="hidden" name="code_challenge" value="{code_challenge}">
<input type="hidden" name="code_challenge_method" value="{ccm}">
{optional_inputs}<input type="hidden" name="csrf" value="{csrf}">
<label for="password">Password</label>
<input id="password" type="password" name="password" autocomplete="current-password" autofocus required>
<button type="submit">Sign in</button>
</form>
</body></html>"#,
        client_id = esc(&q.client_id),
        client_name = esc(client_name),
        redirect_host = esc(&redirect_host),
        response_type = esc(&q.response_type),
        redirect_uri = esc(&q.redirect_uri),
        code_challenge = esc(&q.code_challenge),
        ccm = esc(&q.code_challenge_method),
        csrf = esc(csrf),
    )
}

/// Number of random bytes in each issued code and token.
const TOKEN_BYTES: usize = 32;

/// Return the host and port of `redirect_uri`, which receives the authorization code.
fn redirect_host(redirect_uri: &str) -> String {
    let Ok(url) = Url::parse(redirect_uri) else {
        return redirect_uri.to_string();
    };
    match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_string(),
        (None, _) => redirect_uri.to_string(),
    }
}

fn random_token() -> String {
    let mut buf = [0u8; TOKEN_BYTES];
    rand::rng().fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

/// Return `true` if `token` has the form of a token that [`random_token`] makes.
pub(crate) fn has_issued_form(token: &str) -> bool {
    token.len() == base64::encoded_len(TOKEN_BYTES, false).unwrap_or_default()
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Return the hex SHA-256 digest that keys the stored row of an authorization code or token.
pub fn secret_hash(secret: &str) -> String {
    use std::fmt::Write as _;
    let mut hex = String::with_capacity(64);
    for b in Sha256::digest(secret.as_bytes()) {
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

fn pkce_s256(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// Return `true` if `a` equals `b`, in a time that does not depend on where they differ.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.ct_eq(b).into()
}

/// Key that signs the CSRF tokens of the sign-in form.
struct CsrfKey(Vec<u8>);

/// Return the stored key named `name`, and queue the insert of a new random key if the database holds none.
async fn load_or_create_key(txn: &mut Txn, name: &str) -> Result<String, OAuthError> {
    if let Some(row) = txn.get_oauth::<OAuthKey>(name).await? {
        return Ok(row.key);
    }
    let row = OAuthKey {
        name: name.to_string(),
        key: random_token(),
    };
    txn.insert_lasting_oauth(&row);
    Ok(row.key)
}

fn csrf_mint(key: &CsrfKey, q: &AuthorizeQuery) -> String {
    let ts = Utc::now().timestamp();
    let mac_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(csrf_mac(key, ts, q));
    format!("{ts}.{mac_b64}")
}

fn csrf_verify(key: &CsrfKey, q: &AuthorizeQuery, token: &str) -> bool {
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
    constant_time_eq(&provided, &csrf_mac(key, ts, q))
}

// Length-prefix each field before hashing to prevent canonicalization ambiguity.
fn csrf_mac(key: &CsrfKey, ts: i64, q: &AuthorizeQuery) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(&key.0).expect("HMAC accepts any key length");
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
    #[error("invalid_client")]
    InvalidClient,
    #[error("invalid_scope")]
    InvalidScope,
    #[error("unsupported_response_type")]
    UnsupportedResponseType,
    #[error("unsupported_grant_type: {0}")]
    UnsupportedGrantType(String),
    #[error("too_many_requests")]
    TooManyRequests(Duration),
    #[error(transparent)]
    Internal(#[from] crate::Error),
}

impl OAuthError {
    fn invalid_request(msg: impl Into<String>) -> Self {
        Self::InvalidRequest(msg.into())
    }
}

impl From<crate::db::client::Error> for OAuthError {
    fn from(e: crate::db::client::Error) -> Self {
        OAuthError::Internal(e.into())
    }
}

impl crate::db::client::TxnError for OAuthError {
    fn client_error(&self) -> Option<&crate::db::client::Error> {
        match self {
            OAuthError::Internal(e) => e.client_error(),
            _ => None,
        }
    }
}

impl IntoResponse for OAuthError {
    fn into_response(self) -> Response {
        let mut headers = HeaderMap::new();
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        let (status, code, description) = match &self {
            OAuthError::InvalidRequest(msg) => {
                (StatusCode::BAD_REQUEST, "invalid_request", msg.clone())
            }
            OAuthError::InvalidGrant => (
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "authorization grant is invalid, expired, revoked, or does not match".to_string(),
            ),
            OAuthError::InvalidClient => (
                StatusCode::UNAUTHORIZED,
                "invalid_client",
                "the client is not registered; register again".to_string(),
            ),
            OAuthError::InvalidScope => (
                StatusCode::BAD_REQUEST,
                "invalid_scope",
                "the server grants only the mcp scope".to_string(),
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
            OAuthError::TooManyRequests(wait) => {
                let secs = throttle::retry_after_secs(*wait);
                headers.insert(header::RETRY_AFTER, HeaderValue::from(secs));
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    "too_many_requests",
                    format!("too many requests from this address; try again in {secs} seconds"),
                )
            }
            OAuthError::Internal(e) => {
                tracing::error!(error = %e, "oauth: internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "server_error",
                    "internal server error".to_string(),
                )
            }
        };
        let body = serde_json::json!({
            "error": code,
            "error_description": description,
        });
        (status, headers, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_can_omit_the_scope_or_name_only_mcp() {
        assert!(check_scope(None).is_ok());
        assert!(check_scope(Some("")).is_ok());
        assert!(check_scope(Some("mcp")).is_ok());
        assert!(matches!(
            check_scope(Some("mcp admin")),
            Err(OAuthError::InvalidScope)
        ));
        assert!(matches!(
            check_scope(Some("read")),
            Err(OAuthError::InvalidScope)
        ));
    }

    fn test_db() -> Db {
        let cfg = crate::db::DbConfig {
            url: "http://127.0.0.1:9".into(),
            token: "t".into(),
            db: "d".into(),
            cache_dir: None,
            curriculum: crate::graph::Curriculum::embedded(),
        };
        Db::connect(&cfg).unwrap()
    }

    #[test]
    fn cached_access_tokens_expire_and_stay_bounded() {
        let tokens = AccessTokens::new(test_db());
        let now = Utc::now();
        tokens.remember("valid".into(), now + Duration::minutes(1), now);
        tokens.remember("expired".into(), now - Duration::seconds(1), now);
        assert!(tokens.cached("valid", now));
        assert!(!tokens.cached("expired", now));
        assert!(!tokens.lock().contains_key("expired"), "a lookup evicts it");

        for i in 0..MAX_CACHED_ACCESS_TOKENS {
            let expires_at =
                now + Duration::minutes(2) + Duration::seconds(i64::try_from(i).unwrap());
            tokens.remember(format!("t{i}"), expires_at, now);
        }
        assert_eq!(tokens.lock().len(), MAX_CACHED_ACCESS_TOKENS);
        assert!(
            !tokens.cached("valid", now),
            "the token that expires first leaves"
        );
        assert!(tokens.cached("t0", now));
    }

    #[test]
    fn only_a_token_with_the_issued_form_reaches_the_database() {
        assert!(has_issued_form(&random_token()));
        assert!(!has_issued_form("an-oauth-access-token"));
        assert!(!has_issued_form(&format!("{}=", &random_token()[1..])));
        assert!(!has_issued_form(&"a".repeat(44)));
    }
}
