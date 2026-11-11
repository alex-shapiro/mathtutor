//! Streamable HTTP MCP server for `mt mcp`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    body::Body,
    extract::{ConnectInfo, Query, State},
    http::{self, Request, StatusCode, header::AUTHORIZATION},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    handler::server::{router::prompt::PromptRouter, tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, GetPromptRequestParams, GetPromptResult, Implementation, ListPromptsResult,
        PaginatedRequestParams, PromptMessage, PromptMessageRole, ProtocolVersion,
        ServerCapabilities, ServerInfo,
    },
    prompt, prompt_handler, prompt_router,
    service::RequestContext,
    tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::never::NeverSessionManager,
    },
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use tower_http::trace::DefaultOnResponse;
use url::Url;

use crate::Error;
use crate::db::{Db, DbConfig};
use crate::error::ErrorKind;
use crate::overlay::{QuizDraft, QuizPatch};
use crate::surface::{Surface, Tool};
use crate::types::{Rating, Strategy};
use crate::{answer, discover, path, scheduler, state, store, subpath, syllabus, tree};

// Keep distinct from `Result` because rmcp macros expand to bare `Result<_, ErrorData>`.
type CrateResult<T> = std::result::Result<T, Error>;

pub mod oauth;
mod throttle;

pub use throttle::ClientAddrSource;

#[derive(Clone)]
pub struct MathTutorServer {
    db: Db,
    tool_router: ToolRouter<Self>,
    prompt_router: PromptRouter<Self>,
}

impl MathTutorServer {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            tool_router: Self::tool_router(),
            prompt_router: Self::prompt_router(),
        }
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct NewPathArgs {
    /// Learning goal text.
    pub goal: String,
    /// Target atom or cluster IDs, at least 1.
    pub targets: Vec<String>,
    /// Initial traversal strategy (`bottom_up` or `top_down`).
    #[serde(default)]
    pub strategy: Strategy,
}

#[derive(Deserialize, JsonSchema)]
pub struct PathOnlyArgs {
    pub path_id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetStrategyArgs {
    pub path_id: String,
    /// Traversal strategy (`bottom_up` or `top_down`).
    pub strategy: Strategy,
}

#[derive(Deserialize, JsonSchema)]
pub struct SubpathSetArgs {
    pub path_id: String,
    /// Ordered list of atom IDs ending with a path target.
    pub atoms: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct GetNextArgs {
    pub path_id: String,
    /// Action filter (`default`, `new`, or `due`).
    #[serde(default)]
    pub mode: scheduler::NextMode,
}

#[derive(Deserialize, JsonSchema)]
pub struct GetTreeArgs {
    pub path_id: String,
    /// Maximum tree depth.
    #[serde(default)]
    pub depth: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct GetSyllabusArgs {
    pub path_id: String,
    /// Maximum number of upcoming atoms to return (default 10).
    #[serde(default = "default_syllabus_n")]
    pub n: usize,
}

fn default_syllabus_n() -> usize {
    10
}

#[derive(Deserialize, JsonSchema)]
pub struct GetItemArgs {
    pub id: String,
    #[serde(default)]
    pub path_id: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct GetChildrenArgs {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub path_id: Option<String>,
    /// Whether to include all descendants recursively.
    #[serde(default)]
    pub recursive: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct UpsertLessonArgs {
    pub atom: String,
    pub body: String,
    pub path_id: String,
}

/// Arguments of the `create_quiz` tool.
#[derive(Deserialize, JsonSchema)]
pub struct CreateQuizArgs {
    pub atom: String,
    #[serde(flatten)]
    pub quiz: QuizDraft,
    pub path_id: String,
}

/// Arguments of the `update_quiz` tool.
#[derive(Deserialize, JsonSchema)]
pub struct UpdateQuizArgs {
    pub quiz_id: String,
    #[serde(flatten)]
    pub patch: QuizPatch,
    pub path_id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct DeleteQuizArgs {
    pub quiz_id: String,
    pub path_id: String,
}

/// Arguments of the `answer_quiz` tool.
#[derive(Deserialize, JsonSchema)]
pub struct AnswerQuizArgs {
    pub quiz_id: String,
    /// Verbatim reply of the user.
    #[serde(default)]
    pub user_answer: Option<String>,
    pub rating: Rating,
    pub path_id: String,
}

#[tool_router]
impl MathTutorServer {
    #[tool(description = "List all learning paths for this user.")]
    async fn get_paths(&self) -> std::result::Result<CallToolResult, McpError> {
        encode(Tool::GetPaths, path::compute_path_list(&self.db).await)
    }

    #[tool(description = "Start a new learning path with a goal and targets.")]
    async fn new_path(
        &self,
        Parameters(args): Parameters<NewPathArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let result = path::cmd_path_new(&self.db, &args.goal, &args.targets, args.strategy).await;
        encode(Tool::NewPath, result.map(|id| json!({ "path_id": id })))
    }

    #[tool(description = "Switch a path traversal strategy (bottom_up or top_down).")]
    async fn set_strategy(
        &self,
        Parameters(args): Parameters<SetStrategyArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let result = path::cmd_path_strategy(&self.db, Some(&args.path_id), args.strategy).await;
        encode(
            Tool::SetStrategy,
            result.map(|id| json!({ "path_id": id, "strategy": args.strategy })),
        )
    }

    #[tool(description = "Set the ordered atom subpath for a top-down path ending in a target.")]
    async fn subpath_set(
        &self,
        Parameters(args): Parameters<SubpathSetArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let result = subpath::cmd_subpath_set(&self.db, Some(&args.path_id), &args.atoms).await;
        encode(Tool::SubpathSet, result.map(|id| json!({ "path_id": id })))
    }

    #[tool(description = "Clear the subpath for a path.")]
    async fn subpath_clear(
        &self,
        Parameters(args): Parameters<PathOnlyArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let result = subpath::cmd_subpath_clear(&self.db, Some(&args.path_id)).await;
        encode(
            Tool::SubpathClear,
            result.map(|id| json!({ "path_id": id })),
        )
    }

    #[tool(description = "Get the next lesson or quiz in the path.")]
    async fn get_next(
        &self,
        Parameters(args): Parameters<GetNextArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let result =
            scheduler::compute_next(&self.db, Some(&args.path_id), args.mode, Surface::Mcp).await;
        encode(Tool::GetNext, result)
    }

    #[tool(description = "Return progress summary for a path.")]
    async fn get_state(
        &self,
        Parameters(args): Parameters<PathOnlyArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        encode(
            Tool::GetState,
            state::compute_state(&self.db, Some(&args.path_id)).await,
        )
    }

    #[tool(description = "Return the prerequisite tree and atom status for a path.")]
    async fn get_tree(
        &self,
        Parameters(args): Parameters<GetTreeArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        encode(
            Tool::GetTree,
            tree::compute_tree(&self.db, Some(&args.path_id), args.depth).await,
        )
    }

    #[tool(description = "Preview upcoming lesson topics.")]
    async fn get_syllabus(
        &self,
        Parameters(args): Parameters<GetSyllabusArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        encode(
            Tool::GetSyllabus,
            syllabus::compute_syllabus(&self.db, Some(&args.path_id), args.n).await,
        )
    }

    #[tool(description = "Return details for a curriculum node (atom or cluster).")]
    async fn get_item(
        &self,
        Parameters(args): Parameters<GetItemArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let lookup = discover::Lookup::Db {
            db: &self.db,
            path: args.path_id.as_deref(),
        };
        encode(
            Tool::GetItem,
            discover::compute_item(lookup, &args.id).await,
        )
    }

    #[tool(description = "List children of a curriculum node. Omit `id` for root areas.")]
    async fn get_children(
        &self,
        Parameters(args): Parameters<GetChildrenArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let lookup = discover::Lookup::Db {
            db: &self.db,
            path: args.path_id.as_deref(),
        };
        encode(
            Tool::GetChildren,
            discover::compute_children(lookup, args.id.as_deref(), args.recursive.unwrap_or(false))
                .await,
        )
    }

    #[tool(description = "Create or update a lesson.")]
    async fn upsert_lesson(
        &self,
        Parameters(args): Parameters<UpsertLessonArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let result =
            store::cmd_lesson_upsert(&self.db, &args.atom, args.body, Some(&args.path_id)).await;
        encode(
            Tool::UpsertLesson,
            result.map(|()| json!({ "atom_id": args.atom })),
        )
    }

    #[tool(description = "Create a quiz.")]
    async fn create_quiz(
        &self,
        Parameters(args): Parameters<CreateQuizArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let result =
            store::cmd_quiz_create(&self.db, &args.atom, args.quiz, Some(&args.path_id)).await;
        encode(
            Tool::CreateQuiz,
            result.map(|quiz_id| json!({ "quiz_id": quiz_id })),
        )
    }

    #[tool(description = "Update fields of an existing quiz while preserving ID and FSRS history.")]
    async fn update_quiz(
        &self,
        Parameters(args): Parameters<UpdateQuizArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let result =
            store::cmd_quiz_update(&self.db, &args.quiz_id, &args.patch, Some(&args.path_id)).await;
        encode(
            Tool::UpdateQuiz,
            result.map(|()| json!({ "quiz_id": args.quiz_id })),
        )
    }

    #[tool(description = "Delete a quiz from the curriculum overlay.")]
    async fn delete_quiz(
        &self,
        Parameters(args): Parameters<DeleteQuizArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let result = store::cmd_quiz_delete(&self.db, &args.quiz_id, Some(&args.path_id)).await;
        encode(
            Tool::DeleteQuiz,
            result.map(|()| json!({ "quiz_id": args.quiz_id })),
        )
    }

    #[tool(description = "Record a quiz answer and FSRS rating.")]
    async fn answer_quiz(
        &self,
        Parameters(args): Parameters<AnswerQuizArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let result = answer::cmd_quiz_answer(
            &self.db,
            &args.quiz_id,
            args.rating,
            args.user_answer,
            Some(&args.path_id),
        )
        .await;
        encode(
            Tool::AnswerQuiz,
            result.map(|()| json!({ "quiz_id": args.quiz_id, "rating": args.rating })),
        )
    }
}

#[prompt_router]
impl MathTutorServer {
    #[prompt(
        name = "mathtutor-playbook",
        description = "Master instructions for the tutor agent."
    )]
    async fn playbook(&self) -> std::result::Result<GetPromptResult, McpError> {
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(
            PromptMessageRole::Assistant,
            crate::instruct::playbook(Surface::Mcp),
        )])
        .with_description("Math Tutor agent operator playbook"))
    }
}

// rmcp handler macros generate `async` methods that do not await.
#[allow(unknown_lints, clippy::unused_async_trait_impl)]
#[tool_handler(router = self.tool_router)]
#[prompt_handler(router = self.prompt_router)]
impl ServerHandler for MathTutorServer {
    fn get_info(&self) -> ServerInfo {
        let caps = ServerCapabilities::builder()
            .enable_tools()
            .enable_prompts()
            .build();
        ServerInfo::new(caps)
            .with_protocol_version(ProtocolVersion::V_2025_06_18)
            .with_server_info(Implementation::from_build_env())
            .with_instructions(
                "Math Tutor MCP server. Call the `mathtutor-playbook` prompt for the \
                 full operator playbook before invoking tools.",
            )
    }
}

/// Encode `result` of `tool` as a tool result, and add the safe next step to an error with an unknown outcome.
// Wrap non-object JSON payloads because MCP requires `structuredContent` to be a JSON object.
fn encode<T: Serialize>(
    tool: Tool,
    result: CrateResult<T>,
) -> std::result::Result<CallToolResult, McpError> {
    match result {
        Ok(value) => {
            let json = serde_json::to_value(value)
                .map_err(|e| McpError::internal_error(format!("encode: {e}"), None))?;
            let object = if json.is_object() {
                json
            } else {
                json!({ "value": json })
            };
            Ok(CallToolResult::structured(object))
        }
        Err(e) => {
            let mut body = json!({ "error": e.message(Surface::Mcp), "kind": e.kind().name() });
            if e.outcome_is_unknown() {
                body["next_step"] = json!(tool.operation().unknown_outcome_step(Surface::Mcp));
            }
            Ok(CallToolResult::structured_error(body))
        }
    }
}

/// Authentication configuration for `mt mcp`.
#[derive(Clone, Debug, Default)]
pub struct AuthConfig {
    /// Static bearer token from `MT_API_KEY` or `--api-key`.
    pub api_key: Option<String>,
    /// OAuth administrator password from `MT_ADMIN_PASSWORD` or `--admin-password`.
    pub admin_password: Option<String>,
    /// Public issuer URL from `MT_PUBLIC_URL` or `--public-url`.
    pub public_url: Option<String>,
    /// Source of the client address that limits sign-in attempts and client registrations.
    pub client_addrs: ClientAddrSource,
}

/// Smallest number of characters in `MT_API_KEY`.
const MIN_API_KEY_LEN: usize = 32;

impl AuthConfig {
    fn validate(&self) -> CrateResult<()> {
        if self.api_key.is_none() && self.admin_password.is_none() {
            return Err(Error::MissingAuth);
        }
        if self
            .api_key
            .as_ref()
            .is_some_and(|k| k.chars().count() < MIN_API_KEY_LEN)
        {
            return Err(Error::WeakApiKey {
                min: MIN_API_KEY_LEN,
            });
        }
        Ok(())
    }
}

/// Return the name of every MCP tool.
#[cfg(test)]
pub(crate) fn tool_names() -> Vec<String> {
    tool_fields().into_keys().collect()
}

/// Return the input field names of each MCP tool, keyed by tool name.
#[cfg(test)]
pub(crate) fn tool_fields() -> std::collections::HashMap<String, std::collections::HashSet<String>>
{
    MathTutorServer::tool_router()
        .list_all()
        .into_iter()
        .map(|t| {
            let fields = t
                .input_schema
                .get("properties")
                .and_then(|p| p.as_object())
                .map(|p| p.keys().cloned().collect())
                .unwrap_or_default();
            (t.name.into_owned(), fields)
        })
        .collect()
}

pub fn print_tools() -> CrateResult<()> {
    let tools = MathTutorServer::tool_router().list_all();
    println!("{}", serde_json::to_string_pretty(&tools)?);
    Ok(())
}

/// Run the MCP server on `addr` until SIGINT or SIGTERM.
pub async fn run(addr: &str, auth: AuthConfig, cfg: DbConfig) -> CrateResult<()> {
    auth.validate()?;

    let cfg = DbConfig {
        cache_dir: None,
        ..cfg
    };
    let db = Db::connect(&cfg)?;
    let auth_db = db.with_auth_tables();
    let shutdown = CancellationToken::new();

    let mcp_config = StreamableHttpServerConfig::default()
        .with_stateful_mode(false)
        .with_sse_keep_alive(Some(Duration::from_secs(15)))
        .with_cancellation_token(shutdown.child_token())
        .disable_allowed_hosts();

    let factory = {
        let db = db.clone();
        move || Ok(MathTutorServer::new(db.clone()))
    };

    let mcp_service = StreamableHttpService::new(
        factory,
        Arc::new(NeverSessionManager::default()),
        mcp_config,
    );

    let Ok(socket_addr) = addr.parse() else {
        return Err(Error::BadBindAddr(addr.to_owned()));
    };

    let public_url = resolve_public_url(auth.public_url.as_deref(), socket_addr)?;

    let limits = throttle::AddrLimits::new(&auth_db, auth.client_addrs.clone());
    let auth_state = AuthState::new(&auth, &public_url, auth_db.clone(), limits.clone());
    // Run body logging after auth so unauthenticated requests are not buffered.
    let mut app = Router::new()
        .nest_service("/mcp", mcp_service)
        .route_layer(middleware::from_fn(log_request_body))
        .route_layer(middleware::from_fn_with_state(auth_state, auth_middleware))
        .route("/health", get(health))
        .route("/ready", get(ready).with_state(Readiness::new(db.clone())));

    if let Some(password) = auth.admin_password.as_deref() {
        let oauth_state =
            oauth::OAuthState::new(auth_db, password, public_url.clone(), limits.clone());
        app = app.merge(oauth::router(oauth_state));
    }

    let trace = tower_http::trace::TraceLayer::new_for_http()
        .make_span_with(|req: &Request<Body>| {
            tracing::info_span!("request", method = %req.method(), path = %req.uri().path())
        })
        .on_response(DefaultOnResponse::new().level(tracing::Level::INFO));

    let app = app.layer(trace);

    let listener = tokio::net::TcpListener::bind(socket_addr)
        .await
        .map_err(|e| Error::Bind {
            addr: addr.to_string(),
            source: e,
        })?;
    tracing::info!(%socket_addr, %public_url, oauth = auth.admin_password.is_some(), "mt mcp listening");

    let shutdown_signal = {
        let token = shutdown.clone();
        async move {
            wait_for_shutdown().await;
            token.cancel();
        }
    };

    let app = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal)
        .await
        .map_err(Error::Serve)?;

    tracing::info!("mt mcp shutdown complete");
    Ok(())
}

fn resolve_public_url(explicit: Option<&str>, socket: std::net::SocketAddr) -> CrateResult<Url> {
    let raw = match explicit {
        Some(s) => s.to_string(),
        None => format!("http://{socket}"),
    };
    Url::parse(&raw).map_err(|_| Error::BadPublicUrl(raw))
}

#[derive(Clone)]
struct AuthState {
    api_key: Option<Arc<str>>,
    access_tokens: Option<oauth::AccessTokens>,
    limits: throttle::AddrLimits,
    www_authenticate: Arc<str>,
}

impl AuthState {
    fn new(auth: &AuthConfig, public_url: &Url, db: Db, limits: throttle::AddrLimits) -> Self {
        let api_key = auth
            .api_key
            .as_deref()
            .map(|s| Arc::<str>::from(s.to_string().into_boxed_str()));
        let access_tokens = auth
            .admin_password
            .as_deref()
            .map(|_| oauth::AccessTokens::new(db));
        let issuer = public_url.as_str().trim_end_matches('/');
        let www_authenticate = format!(
            r#"Bearer realm="mt mcp", resource_metadata="{issuer}/.well-known/oauth-protected-resource""#
        );
        Self {
            api_key,
            access_tokens,
            limits,
            www_authenticate: Arc::from(www_authenticate.into_boxed_str()),
        }
    }

    /// Return the `401` response that points the client to the OAuth metadata.
    fn challenge(&self) -> Response {
        let mut resp = Response::new(Body::empty());
        *resp.status_mut() = StatusCode::UNAUTHORIZED;
        if let Ok(value) = http::HeaderValue::from_str(self.www_authenticate.as_ref()) {
            resp.headers_mut()
                .insert(http::header::WWW_AUTHENTICATE, value);
        }
        resp
    }
}

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

/// Admit a request with `MT_API_KEY` or a valid access token, and delay the bearer tokens of a client address after too many rejected tokens.
async fn auth_middleware(
    State(state): State<AuthState>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    Query(q): Query<TokenQuery>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let from_header = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::to_string);
    let Some(token) = from_header.or(q.token) else {
        return state.challenge();
    };
    if let Some(expected) = state.api_key.as_deref()
        && oauth::constant_time_eq(token.as_bytes(), expected.as_bytes())
    {
        return next.run(req).await;
    }
    if let Some(access_tokens) = &state.access_tokens
        && access_tokens.is_cached(&token)
    {
        return next.run(req).await;
    }
    let addr = state.limits.client_addr(peer, req.headers());
    if let Some(wait) = state.limits.bearer_wait(addr) {
        return too_many_requests(wait);
    }
    let Some(access_tokens) = state
        .access_tokens
        .as_ref()
        .filter(|_| oauth::has_issued_form(&token))
    else {
        tracing::warn!(%addr, "bearer token rejected");
        state.limits.record_bearer_failure(addr);
        return state.challenge();
    };
    match access_tokens.is_valid(&token).await {
        Ok(true) => next.run(req).await,
        Ok(false) => {
            tracing::warn!(%addr, "access token rejected");
            state.limits.record_bearer_failure(addr);
            state.challenge()
        }
        Err(e) => {
            tracing::warn!(error = %e, "cannot validate access token");
            token_check_unavailable()
        }
    }
}

/// Return the `429` response for a client address that must wait `wait`.
fn too_many_requests(wait: chrono::Duration) -> Response {
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::TOO_MANY_REQUESTS;
    resp.headers_mut().insert(
        http::header::RETRY_AFTER,
        http::HeaderValue::from(throttle::retry_after_secs(wait)),
    );
    resp
}

fn token_check_unavailable() -> Response {
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
    resp.headers_mut().insert(
        http::header::RETRY_AFTER,
        http::HeaderValue::from_static("5"),
    );
    resp
}

const MAX_LOGGED_BODY: usize = 1 << 20;

async fn log_request_body(req: Request<Body>, next: Next) -> Response {
    if !tracing::enabled!(tracing::Level::DEBUG) {
        return next.run(req).await;
    }
    let too_big = req
        .headers()
        .get(http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok())
        .is_some_and(|n| n > MAX_LOGGED_BODY);
    if too_big {
        tracing::debug!("mcp request body too large to log");
        return next.run(req).await;
    }
    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_LOGGED_BODY).await {
        Ok(b) => b,
        Err(e) => {
            // Return 400 because the request body stream has already been consumed.
            tracing::warn!(error = %e, "mcp request body buffering failed");
            let mut resp = Response::new(Body::empty());
            *resp.status_mut() = StatusCode::BAD_REQUEST;
            return resp;
        }
    };
    if let Ok(s) = std::str::from_utf8(&bytes) {
        tracing::debug!(body = %s, "mcp request body");
    } else {
        tracing::debug!(bytes = bytes.len(), "mcp request body (non-utf8)");
    }
    next.run(Request::from_parts(parts, Body::from(bytes)))
        .await
}

async fn health() -> &'static str {
    "ok"
}

/// Time that the server reuses the result of a readiness check, so that `/ready` sends at most one Baserunna request in each interval.
const READY_CHECK_INTERVAL: Duration = Duration::from_secs(10);

/// Longest time that a readiness check waits for the database before it reports a failure.
const READY_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// Readiness check of the database, with the result of the last check.
#[derive(Clone)]
struct Readiness {
    db: Db,
    last: Arc<std::sync::Mutex<Option<ReadyCheck>>>,
    /// A request runs a check now.
    checking: Arc<AtomicBool>,
}

/// Result of one readiness check.
#[derive(Clone, Copy)]
struct ReadyCheck {
    at: Instant,
    /// Kind of the error if the database was not usable.
    failure: Option<ErrorKind>,
}

impl Readiness {
    fn new(db: Db) -> Self {
        Self {
            db,
            last: Arc::default(),
            checking: Arc::default(),
        }
    }

    /// Return the kind of the error if the database is not usable, from the last check if it is recent or if another request runs a check now.
    async fn failure(&self) -> Option<ErrorKind> {
        let last = *self.lock();
        if let Some(check) = last.filter(|c| c.at.elapsed() < READY_CHECK_INTERVAL) {
            return check.failure;
        }
        let claim = CheckClaim::take(&self.checking);
        if claim.is_none()
            && let Some(check) = last
        {
            return check.failure;
        }
        let failure = match tokio::time::timeout(READY_CHECK_TIMEOUT, self.db.check_usable()).await
        {
            Ok(Ok(())) => None,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "readiness check failed");
                Some(e.kind())
            }
            Err(_) => {
                tracing::warn!("readiness check timed out");
                Some(ErrorKind::Database)
            }
        };
        *self.lock() = Some(ReadyCheck {
            at: Instant::now(),
            failure,
        });
        failure
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<ReadyCheck>> {
        self.last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Claim of the one readiness check that runs at a time, released when the check ends or its request stops.
struct CheckClaim<'a>(&'a AtomicBool);

impl<'a> CheckClaim<'a> {
    /// Claim `checking`, or return `None` if another request holds it.
    fn take(checking: &'a AtomicBool) -> Option<Self> {
        checking
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self(checking))
    }
}

impl Drop for CheckClaim<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Return `200` if the tools can use the database, or `503` with the error kind.
async fn ready(State(readiness): State<Readiness>) -> Response {
    match readiness.failure().await {
        None => (StatusCode::OK, "ready").into_response(),
        Some(kind) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "ready": false, "kind": kind.name() })),
        )
            .into_response(),
    }
}

async fn wait_for_shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    {
        let Ok(mut term) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        else {
            ctrl_c.await;
            return;
        };
        tokio::select! {
            () = ctrl_c => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        ctrl_c.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use baserunna_client::TransportError;

    fn lost_commit() -> Error {
        Error::Baserunna(baserunna_client::Error::Transport(TransportError::Unknown(
            "reset".into(),
        )))
    }

    #[test]
    fn every_mcp_tool_is_an_agent_command() {
        let mut served = tool_names();
        served.sort();
        let mut commands: Vec<String> = Tool::ALL.iter().map(|t| t.mcp().to_string()).collect();
        commands.sort();
        assert_eq!(served, commands);
    }

    #[test]
    fn unknown_outcome_error_names_the_safe_next_call() {
        let result = encode::<()>(Tool::AnswerQuiz, Err(lost_commit())).unwrap();
        let body = result.structured_content.expect("structured error");
        assert_eq!(body["kind"], json!("outcome_unknown"));
        assert_eq!(
            body["next_step"],
            json!(crate::surface::Operation::AnswerQuiz.unknown_outcome_step(Surface::Mcp))
        );
    }

    #[test]
    fn every_mcp_step_matches_the_input_schema_of_its_tool() {
        let tools = MathTutorServer::tool_router().list_all();
        for step in crate::instruct::filled_steps() {
            let shown = step.render(Surface::Mcp);
            let tool = tools
                .iter()
                .find(|t| t.name == step.tool().mcp())
                .unwrap_or_else(|| panic!("{shown}: unknown tool"));
            let properties = tool.input_schema["properties"].as_object().unwrap();
            let args = step.mcp_args();
            for (name, value) in &args {
                let property = properties
                    .get(name)
                    .unwrap_or_else(|| panic!("{shown}: unknown field {name}"));
                if let Some(options) = property.get("enum") {
                    let options = options.as_array().unwrap();
                    assert!(options.contains(value), "{shown}: {name} = {value}");
                }
            }
            for name in tool.input_schema["required"].as_array().unwrap() {
                let name = name.as_str().unwrap();
                assert!(args.contains_key(name), "{shown}: missing {name}");
            }
        }
    }

    #[test]
    fn the_fly_app_names_its_baserunna_server() {
        let fly = include_str!("../../fly.toml");
        let env = fly
            .split("[env]")
            .nth(1)
            .unwrap()
            .split("\n[")
            .next()
            .unwrap();
        assert!(
            env.lines()
                .any(|l| l.starts_with("BASERUNNA_URL = 'https://")),
            "{env}"
        );
    }

    #[test]
    fn a_short_api_key_stops_the_server() {
        let auth = |key: &str| AuthConfig {
            api_key: Some(key.into()),
            ..AuthConfig::default()
        };
        let e = auth("short").validate().unwrap_err();
        assert!(matches!(e, Error::WeakApiKey { min: 32 }), "{e:?}");
        assert!(e.to_string().contains("MT_API_KEY"), "{e}");
        assert_eq!(e.exit_code(), 2);
        auth(&"k".repeat(MIN_API_KEY_LEN)).validate().unwrap();
    }

    #[test]
    fn known_outcome_error_omits_next_step() {
        let result = encode::<()>(Tool::AnswerQuiz, Err(Error::NoPath(None))).unwrap();
        let body = result.structured_content.expect("structured error");
        assert!(body.get("next_step").is_none());
    }
}
