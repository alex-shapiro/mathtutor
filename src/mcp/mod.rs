//! Streamable HTTP MCP server (`mt mcp`).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    body::Body,
    extract::{Query, State},
    http::{self, Request, StatusCode, header::AUTHORIZATION},
    middleware::{self, Next},
    response::Response,
    routing::get,
};
use chrono::{DateTime, Utc};
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
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse};
use url::Url;

use crate::Error;
use crate::db::{Db, DbConfig};
use crate::graph::{Graph, natural_id_cmp};
use crate::progress::PathProgress;
use crate::types::{Difficulty, QuizType, Rating, Strategy};
use crate::view::{PathSel, Want};
use crate::{answer, discover, graph, path, scheduler, state, store, subpath, syllabus, tree};

// Keep distinct from `Result` because rmcp macros expand to bare `Result<_, ErrorData>`.
type CrateResult<T> = std::result::Result<T, Error>;

pub mod oauth;

const PLAYBOOK: &str = include_str!("playbook.md");

#[derive(Clone)]
pub struct MathTutorServer {
    db: Db,
    graph_dir: Option<Arc<PathBuf>>,
    tool_router: ToolRouter<Self>,
    prompt_router: PromptRouter<Self>,
}

impl MathTutorServer {
    pub fn new(db: Db, graph_dir: Option<PathBuf>) -> Self {
        Self {
            db,
            graph_dir: graph_dir.map(Arc::new),
            tool_router: Self::tool_router(),
            prompt_router: Self::prompt_router(),
        }
    }

    fn graph_path(&self) -> Option<&std::path::Path> {
        self.graph_dir.as_deref().map(AsRef::as_ref)
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct NewPathArgs {
    /// Learning goal text.
    pub goal: String,
    /// Target atom or cluster IDs.
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

#[derive(Deserialize, JsonSchema)]
pub struct CreateQuizArgs {
    pub atom: String,
    pub question: String,
    pub answer: String,
    #[serde(default)]
    pub rubric: Option<String>,
    pub difficulty: Difficulty,
    #[serde(default)]
    pub kind: Option<QuizType>,
    pub path_id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct UpdateQuizArgs {
    pub quiz_id: String,
    #[serde(default)]
    pub question: Option<String>,
    #[serde(default)]
    pub answer: Option<String>,
    #[serde(default)]
    pub rubric: Option<String>,
    #[serde(default)]
    pub difficulty: Option<Difficulty>,
    #[serde(default)]
    pub kind: Option<QuizType>,
    pub path_id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct DeleteQuizArgs {
    pub quiz_id: String,
    pub path_id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct AnswerQuizArgs {
    pub quiz_id: String,
    #[serde(default)]
    pub answer: Option<String>,
    pub rating: Rating,
    pub path_id: String,
}

#[tool_router]
impl MathTutorServer {
    #[tool(description = "List all learning paths for this user.")]
    async fn get_paths(&self) -> std::result::Result<CallToolResult, McpError> {
        encode(
            list_paths(&self.db)
                .await
                .map(|paths| json!({ "paths": paths })),
        )
    }

    #[tool(description = "Start a new learning path with a goal and targets.")]
    async fn new_path(
        &self,
        Parameters(args): Parameters<NewPathArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let result = path::cmd_path_new(
            &self.db,
            &args.goal,
            &args.targets,
            args.strategy,
            self.graph_path(),
        )
        .await;
        encode(result.map(|id| json!({ "path_id": id })))
    }

    #[tool(description = "Switch a path traversal strategy (bottom_up or top_down).")]
    async fn set_strategy(
        &self,
        Parameters(args): Parameters<SetStrategyArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let result = path::cmd_path_strategy(&self.db, Some(&args.path_id), args.strategy).await;
        encode(result.map(|id| json!({ "path_id": id, "strategy": args.strategy })))
    }

    #[tool(description = "Set the ordered atom subpath for a top-down path ending in a target.")]
    async fn subpath_set(
        &self,
        Parameters(args): Parameters<SubpathSetArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let result = subpath::cmd_subpath_set(
            &self.db,
            Some(&args.path_id),
            &args.atoms,
            self.graph_path(),
        )
        .await;
        encode(result.map(|id| json!({ "path_id": id })))
    }

    #[tool(description = "Clear the subpath for a path.")]
    async fn subpath_clear(
        &self,
        Parameters(args): Parameters<PathOnlyArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let result = subpath::cmd_subpath_clear(&self.db, Some(&args.path_id)).await;
        encode(result.map(|id| json!({ "path_id": id })))
    }

    #[tool(description = "Get the next lesson or quiz in the path.")]
    async fn get_next(
        &self,
        Parameters(args): Parameters<GetNextArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let result =
            scheduler::compute_next(&self.db, Some(&args.path_id), self.graph_path(), args.mode)
                .await;
        encode(result)
    }

    #[tool(description = "Return progress summary for a path.")]
    async fn get_state(
        &self,
        Parameters(args): Parameters<PathOnlyArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        encode(state::compute_state(&self.db, Some(&args.path_id), self.graph_path()).await)
    }

    #[tool(description = "Return the prerequisite tree and atom status for a path.")]
    async fn get_tree(
        &self,
        Parameters(args): Parameters<GetTreeArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        encode(compute_tree(&self.db, &args.path_id, args.depth, self.graph_path()).await)
    }

    #[tool(description = "Preview upcoming lesson topics.")]
    async fn get_syllabus(
        &self,
        Parameters(args): Parameters<GetSyllabusArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        encode(
            syllabus::compute_syllabus(&self.db, Some(&args.path_id), args.n, self.graph_path())
                .await,
        )
    }

    #[tool(description = "Return details for a curriculum node (atom or cluster).")]
    async fn get_item(
        &self,
        Parameters(args): Parameters<GetItemArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        encode(
            compute_item(
                &self.db,
                &args.id,
                args.path_id.as_deref(),
                self.graph_path(),
            )
            .await,
        )
    }

    #[tool(description = "List children of a curriculum node. Omit `id` for root areas.")]
    async fn get_children(
        &self,
        Parameters(args): Parameters<GetChildrenArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        encode(
            compute_children(
                &self.db,
                args.id.as_deref(),
                args.path_id.as_deref(),
                args.recursive.unwrap_or(false),
                self.graph_path(),
            )
            .await,
        )
    }

    #[tool(description = "Create or update a lesson.")]
    async fn upsert_lesson(
        &self,
        Parameters(args): Parameters<UpsertLessonArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let result = store::cmd_lesson_upsert(
            &self.db,
            &args.atom,
            args.body,
            Some(&args.path_id),
            self.graph_path(),
        )
        .await;
        encode(result.map(|()| json!({ "atom_id": args.atom })))
    }

    #[tool(description = "Create a quiz.")]
    async fn create_quiz(
        &self,
        Parameters(args): Parameters<CreateQuizArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let result = store::cmd_quiz_create(
            &self.db,
            &args.atom,
            args.difficulty,
            args.question,
            args.answer,
            args.rubric,
            args.kind.unwrap_or_default(),
            Some(&args.path_id),
            self.graph_path(),
        )
        .await;
        encode(result.map(|quiz_id| json!({ "quiz_id": quiz_id })))
    }

    #[tool(description = "Update fields of an existing quiz while preserving ID and FSRS history.")]
    async fn update_quiz(
        &self,
        Parameters(args): Parameters<UpdateQuizArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let result = store::cmd_quiz_update(
            &self.db,
            &args.quiz_id,
            args.question,
            args.answer,
            args.rubric,
            args.difficulty,
            args.kind,
            Some(&args.path_id),
            self.graph_path(),
        )
        .await;
        encode(result.map(|()| json!({ "quiz_id": args.quiz_id })))
    }

    #[tool(description = "Delete a quiz from the curriculum overlay.")]
    async fn delete_quiz(
        &self,
        Parameters(args): Parameters<DeleteQuizArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let result = store::cmd_quiz_delete(
            &self.db,
            &args.quiz_id,
            Some(&args.path_id),
            self.graph_path(),
        )
        .await;
        encode(result.map(|()| json!({ "quiz_id": args.quiz_id })))
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
            args.answer,
            Some(&args.path_id),
            self.graph_path(),
        )
        .await;
        encode(result.map(|()| json!({ "quiz_id": args.quiz_id, "rating": args.rating })))
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
            PLAYBOOK,
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

#[derive(Serialize)]
struct PathSummary {
    id: String,
    goal: String,
    strategy: Strategy,
    created_at: DateTime<Utc>,
    targets: Vec<String>,
}

async fn list_paths(db: &Db) -> CrateResult<Vec<PathSummary>> {
    let want = Want {
        path: PathSel::None,
        overlay: false,
        all_paths: true,
    };
    let view = db.snapshot(&want).await?;
    Ok(view
        .paths
        .into_iter()
        .map(|p| PathSummary {
            id: p.file.id,
            goal: p.file.goal,
            strategy: p.file.strategy,
            created_at: p.file.created_at,
            targets: p.file.targets,
        })
        .collect())
}

#[derive(Serialize)]
struct ItemView {
    item: discover::ShowView,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<AtomStatus>,
}

#[derive(Serialize)]
struct AtomStatus {
    lesson_taught: bool,
    complete: bool,
}

async fn compute_item(
    db: &Db,
    id: &str,
    path_id: Option<&str>,
    graph_dir: Option<&std::path::Path>,
) -> CrateResult<ItemView> {
    let (g, progress) = discover::graph_for(db, path_id, graph_dir).await?;
    let item = discover::show_view(&g, id)?;
    let status = progress.and_then(|p| status_for(&g, id, &p));
    Ok(ItemView { item, status })
}

#[derive(Serialize)]
struct ChildrenView {
    view: discover::ListView,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    descendants: Vec<ChildStatus>,
}

#[derive(Serialize)]
struct ChildStatus {
    id: String,
    name: String,
    is_atom: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<AtomStatus>,
}

async fn compute_children(
    db: &Db,
    id: Option<&str>,
    path_id: Option<&str>,
    recursive: bool,
    graph_dir: Option<&std::path::Path>,
) -> CrateResult<ChildrenView> {
    let (g, progress) = discover::graph_for(db, path_id, graph_dir).await?;
    let manifest = graph::load_manifest_default(graph_dir)?;
    let view = discover::list_view(&g, &manifest, id)?;
    let descendants = if recursive {
        let roots = match id {
            Some(s) => vec![s.to_string()],
            None => manifest.areas.iter().map(|a| a.prefix.clone()).collect(),
        };
        gather_descendants(&g, &roots, progress.as_ref())
    } else {
        Vec::new()
    };
    Ok(ChildrenView { view, descendants })
}

fn gather_descendants(
    g: &Graph,
    roots: &[String],
    progress: Option<&PathProgress>,
) -> Vec<ChildStatus> {
    let mut out = Vec::new();
    let mut stack: Vec<String> = roots.to_vec();
    while let Some(id) = stack.pop() {
        let Some(c) = g.by_id.get(&id) else { continue };
        for child in &c.children_ids {
            stack.push(child.clone());
        }
        out.push(ChildStatus {
            id: c.id.clone(),
            name: c.name.clone(),
            is_atom: c.children_ids.is_empty(),
            status: progress.and_then(|p| status_for(g, &id, p)),
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn status_for(g: &Graph, id: &str, progress: &PathProgress) -> Option<AtomStatus> {
    let c = g.by_id.get(id)?;
    if !c.children_ids.is_empty() {
        return None;
    }
    Some(AtomStatus {
        lesson_taught: progress.lesson_taught(id),
        complete: scheduler::is_atom_complete(g, progress, id),
    })
}

#[derive(Serialize)]
struct TreeView {
    path: String,
    goal: String,
    targets: TreeProgress,
    reachable: ReachProgress,
    nodes: Vec<TreeNode>,
}

#[derive(Serialize)]
struct TreeProgress {
    total: usize,
    learned: usize,
    learned_pct: usize,
}

#[derive(Serialize)]
struct ReachProgress {
    total: usize,
    taught: usize,
    learned: usize,
}

#[derive(Serialize)]
struct TreeNode {
    id: String,
    name: String,
    is_atom: bool,
    is_target: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<AtomStatus>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    children: Vec<TreeNode>,
}

async fn compute_tree(
    db: &Db,
    path_id: &str,
    depth: Option<u32>,
    graph_dir: Option<&std::path::Path>,
) -> CrateResult<TreeView> {
    let view = db.snapshot(&Want::path(Some(path_id))).await?;
    let ps = view.path()?;
    let p = ps.file.clone();
    let g = view.graph(graph_dir)?;
    let manifest = graph::load_manifest_default(graph_dir)?;
    let progress = ps.progress();
    let target_atoms = p.resolve_targets(&g)?;

    let reachable = g.reachable_atoms(&target_atoms);
    let spine = tree::build_spine(&g, &reachable);

    let targets_total = target_atoms.len();
    let targets_learned = target_atoms
        .iter()
        .filter(|a| scheduler::is_atom_complete(&g, &progress, a))
        .count();
    let targets_pct = (targets_learned * 100)
        .checked_div(targets_total)
        .unwrap_or(0);
    let reach_total = reachable.len();
    let reach_taught = reachable
        .iter()
        .filter(|a| progress.lesson_taught(a))
        .count();
    let reach_learned = reachable
        .iter()
        .filter(|a| scheduler::is_atom_complete(&g, &progress, a))
        .count();

    let target_set: std::collections::HashSet<&str> =
        target_atoms.iter().map(String::as_str).collect();

    let mut nodes = Vec::new();
    for area in &manifest.areas {
        let mut top: Vec<TreeNode> = g
            .by_id
            .values()
            .filter(|c| {
                let parts: Vec<&str> = c.id.split('.').collect();
                parts.len() == 2 && parts[0] == area.prefix && spine.contains(&c.id)
            })
            .map(|c| build_tree_node(&g, &progress, &spine, &target_set, c, depth, 1))
            .collect();
        top.sort_by(|a, b| natural_id_cmp(&a.id, &b.id));
        nodes.extend(top);
    }

    Ok(TreeView {
        path: p.id,
        goal: p.goal,
        targets: TreeProgress {
            total: targets_total,
            learned: targets_learned,
            learned_pct: targets_pct,
        },
        reachable: ReachProgress {
            total: reach_total,
            taught: reach_taught,
            learned: reach_learned,
        },
        nodes,
    })
}

fn build_tree_node(
    g: &Graph,
    progress: &PathProgress,
    spine: &std::collections::HashSet<String>,
    targets: &std::collections::HashSet<&str>,
    c: &graph::FlatConcept,
    max_depth: Option<u32>,
    depth: u32,
) -> TreeNode {
    let is_atom = c.children_ids.is_empty();
    let status = if is_atom {
        Some(AtomStatus {
            lesson_taught: progress.lesson_taught(&c.id),
            complete: scheduler::is_atom_complete(g, progress, &c.id),
        })
    } else {
        None
    };
    let children = if max_depth.is_some_and(|md| depth >= md) {
        Vec::new()
    } else {
        let mut kids: Vec<TreeNode> = c
            .children_ids
            .iter()
            .filter_map(|cid| g.by_id.get(cid))
            .filter(|kc| spine.contains(&kc.id))
            .map(|kc| build_tree_node(g, progress, spine, targets, kc, max_depth, depth + 1))
            .collect();
        kids.sort_by(|a, b| natural_id_cmp(&a.id, &b.id));
        kids
    };
    TreeNode {
        id: c.id.clone(),
        name: c.name.clone(),
        is_atom,
        is_target: targets.contains(c.id.as_str()),
        status,
        children,
    }
}

// Wrap non-object JSON payloads because MCP requires `structuredContent` to be a JSON object.
fn encode<T: Serialize>(result: CrateResult<T>) -> std::result::Result<CallToolResult, McpError> {
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
            let body = json!({ "error": format!("{e}"), "kind": error_kind(&e) });
            Ok(CallToolResult::structured_error(body))
        }
    }
}

fn error_kind(e: &Error) -> &'static str {
    match e {
        Error::UnknownId(_) => "unknown_id",
        Error::EmptyCluster(_) => "empty_cluster",
        Error::Cycle => "cycle",
        Error::NotAtom(_) => "not_atom",
        Error::AtomNotFound(_) => "atom_not_found",
        Error::NoPath => "no_path",
        Error::NoLesson(_) => "no_lesson",
        Error::InvalidRating(_) => "invalid_rating",
        Error::InvalidDifficulty(_) => "invalid_difficulty",
        Error::InvalidQuizType(_) => "invalid_quiz_type",
        Error::InvalidStrategy(_) => "invalid_strategy",
        Error::SubpathNotTopDown => "subpath_not_top_down",
        Error::SubpathEmpty => "subpath_empty",
        Error::SubpathTailNotTarget(_) => "subpath_tail_not_target",
        Error::SubpathDuplicateAtom(_) => "subpath_duplicate_atom",
        Error::CardsCorrupt(_) => "cards_corrupt",
        Error::NoHome => "no_home",
        Error::FileIo { .. } | Error::AymlParse { .. } | Error::AymlSerialize(_) => "io",
        Error::Api(_) | Error::Http(_) | Error::MissingToken => "database",
        Error::BadResponse(_) | Error::BadRow(_) | Error::BadEvent(_) => "corrupt",
        Error::Json(_) => "json",
        Error::Fsrs(_) => "fsrs",
        Error::ConflictingFlags(_, _) => "conflicting_flags",
        Error::MissingTursoUrl | Error::Turso(_) | Error::ImportMismatch(_) => "import",
        Error::MissingAuth
        | Error::BadBindAddr { .. }
        | Error::BadPublicUrl { .. }
        | Error::Bind { .. }
        | Error::Serve(_) => "server",
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
}

impl AuthConfig {
    fn validate(&self) -> CrateResult<()> {
        if self.api_key.is_none() && self.admin_password.is_none() {
            return Err(Error::MissingAuth);
        }
        Ok(())
    }
}

pub fn print_tools() -> CrateResult<()> {
    let tools = MathTutorServer::tool_router().list_all();
    println!("{}", serde_json::to_string_pretty(&tools)?);
    Ok(())
}

/// Run the MCP server on `addr` until SIGINT or SIGTERM.
pub async fn run(
    addr: &str,
    auth: AuthConfig,
    cfg: DbConfig,
    graph_dir: Option<PathBuf>,
) -> CrateResult<()> {
    auth.validate()?;

    let db = Db::open(&cfg).await?;
    let shutdown = CancellationToken::new();

    let mcp_config = StreamableHttpServerConfig::default()
        .with_sse_keep_alive(Some(Duration::from_secs(15)))
        .with_cancellation_token(shutdown.child_token())
        .disable_allowed_hosts();

    let factory = {
        let db = db.clone();
        let graph_dir = graph_dir.clone();
        move || Ok(MathTutorServer::new(db.clone(), graph_dir.clone()))
    };

    let mcp_service = StreamableHttpService::new(
        factory,
        Arc::new(LocalSessionManager::default()),
        mcp_config,
    );

    let Ok(socket_addr) = addr.parse() else {
        return Err(Error::BadBindAddr(addr.to_owned()));
    };

    let public_url = resolve_public_url(auth.public_url.as_deref(), socket_addr)?;

    let auth_state = AuthState::new(&auth, &public_url, db.clone());
    // Run body logging after auth so unauthenticated requests are not buffered.
    let mut app = Router::new()
        .nest_service("/mcp", mcp_service)
        .route_layer(middleware::from_fn(log_request_body))
        .route_layer(middleware::from_fn_with_state(auth_state, auth_middleware))
        .route("/health", get(health));

    if let Some(password) = auth.admin_password.as_deref() {
        let oauth_state = oauth::OAuthState::new(db.clone(), password, public_url.clone());
        app = app.merge(oauth::router(oauth_state));
    }

    let trace = tower_http::trace::TraceLayer::new_for_http()
        .make_span_with(DefaultMakeSpan::new().level(tracing::Level::INFO))
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
    db: Option<Db>,
    www_authenticate: Arc<str>,
}

impl AuthState {
    fn new(auth: &AuthConfig, public_url: &Url, db: Db) -> Self {
        let api_key = auth
            .api_key
            .as_deref()
            .map(|s| Arc::<str>::from(s.to_string().into_boxed_str()));
        let db = auth.admin_password.as_deref().map(|_| db);
        let issuer = public_url.as_str().trim_end_matches('/');
        let www_authenticate = format!(
            r#"Bearer realm="mt mcp", resource_metadata="{issuer}/.well-known/oauth-protected-resource""#
        );
        Self {
            api_key,
            db,
            www_authenticate: Arc::from(www_authenticate.into_boxed_str()),
        }
    }
}

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

async fn auth_middleware(
    State(state): State<AuthState>,
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
    let provided = from_header.or(q.token);

    if let Some(token) = provided {
        if let Some(expected) = state.api_key.as_deref()
            && constant_time_eq(token.as_bytes(), expected.as_bytes())
        {
            return next.run(req).await;
        }
        if let Some(db) = state.db.as_ref()
            && let Ok(Some(_client_id)) = oauth::validate_access_token(db, &token).await
        {
            return next.run(req).await;
        }
    }

    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::UNAUTHORIZED;
    if let Ok(value) = http::HeaderValue::from_str(state.www_authenticate.as_ref()) {
        resp.headers_mut()
            .insert(http::header::WWW_AUTHENTICATE, value);
    }
    resp
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.ct_eq(b).into()
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
