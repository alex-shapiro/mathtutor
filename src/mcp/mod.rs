//! `mt mcp`: SSE-over-HTTP MCP server.
//!
//! The CLI ports the same scheduling + storage layer used by the CLI; the
//! server adds a JSON-RPC tool surface, a `mathtutor-playbook` prompt, and
//! a `Bearer`-token (or `?token=` query parameter) auth wrapper.
//!
//! Transport: rmcp's `StreamableHttpService`. The MCP 2025-06-18 Streamable
//! HTTP transport uses `text/event-stream` SSE for server-initiated frames
//! and `application/json` for client POSTs, with 15-second SSE keep-alive
//! comments. That's the SSE-over-HTTP the design doc specifies; rmcp 1.7
//! ships no separate "pure SSE" feature.
//!
//! Database lifecycle: the server holds one shared `libsql::Database`
//! across every session. Each tool call opens a fresh connection, so
//! concurrent calls are safe. After any state-mutating tool, a background
//! `db::maybe_sync` task is spawned; on graceful shutdown a final
//! foreground sync runs before the process exits.

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
use libsql::{Connection, Database};
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
use crate::db::{self, DbConfig};
use crate::graph::Graph;
use crate::progress::PathProgress;
use crate::types::{Difficulty, QuizType, Rating};
use crate::{answer, discover, graph, path, scheduler, state, store, tree};

/// Local alias kept distinct from `std::result::Result` so the rmcp tool /
/// prompt macros (which expand to bare `Result<…, ErrorData>`) don't pick
/// it up by accident.
type CrateResult<T> = std::result::Result<T, Error>;

pub mod oauth;

const PLAYBOOK: &str = include_str!("playbook.md");

/// Background sync cadence for the embedded Turso replica. State-modifying
/// tools also fire a non-blocking sync immediately after success; this
/// interval covers the idle path.
const BACKGROUND_SYNC_INTERVAL: Duration = Duration::from_secs(300);

// ───────────────────────────── server ─────────────────────────────

#[derive(Clone)]
pub struct MathTutorServer {
    db: Arc<Database>,
    cfg: Arc<DbConfig>,
    graph_dir: Option<Arc<PathBuf>>,
    tool_router: ToolRouter<Self>,
    prompt_router: PromptRouter<Self>,
}

impl MathTutorServer {
    pub fn new(db: Arc<Database>, cfg: Arc<DbConfig>, graph_dir: Option<PathBuf>) -> Self {
        Self {
            db,
            cfg,
            graph_dir: graph_dir.map(Arc::new),
            tool_router: Self::tool_router(),
            prompt_router: Self::prompt_router(),
        }
    }

    fn graph_path(&self) -> Option<&std::path::Path> {
        self.graph_dir.as_deref().map(AsRef::as_ref)
    }

    /// Open a fresh connection. `db::connect` enables FK enforcement so
    /// callers get the same pragmas the CLI uses.
    async fn conn(&self) -> std::result::Result<Connection, McpError> {
        db::connect(&self.db)
            .await
            .map_err(|e| map_protocol_error(&e))
    }

    /// Non-blocking sync after a state-modifying tool. Fire-and-forget —
    /// failures show up in stderr; libSQL retries on the next sync.
    fn spawn_sync(&self) {
        let db = self.db.clone();
        let cfg = self.cfg.clone();
        tokio::spawn(async move {
            db::maybe_sync(&db, &cfg).await;
        });
    }
}

// ───────────────────────────── parameter types ────────────────────

#[derive(Deserialize, JsonSchema)]
pub struct NewPathArgs {
    /// Free-text learning goal, e.g. "Understand SVD".
    pub goal: String,
    /// Target atom / cluster / area IDs (one or more).
    pub atoms: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct PathOnlyArgs {
    pub path_id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct GetTreeArgs {
    pub path_id: String,
    /// Max levels to traverse. Omit for full tree.
    #[serde(default)]
    pub depth: Option<u32>,
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
    /// If true, recursively list all descendants.
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

// ───────────────────────────── tools ──────────────────────────────

#[tool_router]
impl MathTutorServer {
    #[tool(description = "List all learning paths for this user.")]
    async fn get_paths(&self) -> std::result::Result<CallToolResult, McpError> {
        let conn = self.conn().await?;
        encode(
            list_paths(&conn)
                .await
                .map(|paths| json!({ "paths": paths })),
        )
    }

    #[tool(description = "Start a new learning path with a goal and target atoms.")]
    async fn new_path(
        &self,
        Parameters(args): Parameters<NewPathArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let conn = self.conn().await?;
        let result = path::cmd_path_new(&conn, &args.goal, &args.atoms, self.graph_path()).await;
        if result.is_ok() {
            self.spawn_sync();
        }
        encode(result.map(|id| json!({ "path_id": id })))
    }

    #[tool(description = "Get the next action (lesson or quiz) in the path.")]
    async fn get_next(
        &self,
        Parameters(args): Parameters<PathOnlyArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let conn = self.conn().await?;
        let result = scheduler::compute_next(&conn, Some(&args.path_id), self.graph_path()).await;
        if result.is_ok() {
            self.spawn_sync();
        }
        encode(result)
    }

    #[tool(description = "Summary of progress for a path: completed vs. remaining atoms.")]
    async fn get_state(
        &self,
        Parameters(args): Parameters<PathOnlyArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let conn = self.conn().await?;
        encode(state::compute_state(&conn, Some(&args.path_id), self.graph_path()).await)
    }

    #[tool(description = "Full prerequisite tree with per-atom status for a path.")]
    async fn get_tree(
        &self,
        Parameters(args): Parameters<GetTreeArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let conn = self.conn().await?;
        encode(compute_tree(&conn, &args.path_id, args.depth, self.graph_path()).await)
    }

    #[tool(description = "Detailed view of a curriculum node (atom, cluster, or area).")]
    async fn get_item(
        &self,
        Parameters(args): Parameters<GetItemArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let conn = self.conn().await?;
        encode(compute_item(&conn, &args.id, args.path_id.as_deref(), self.graph_path()).await)
    }

    #[tool(description = "List children of a curriculum node. Omit `id` for root areas.")]
    async fn get_children(
        &self,
        Parameters(args): Parameters<GetChildrenArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let conn = self.conn().await?;
        encode(
            compute_children(
                &conn,
                args.id.as_deref(),
                args.path_id.as_deref(),
                args.recursive.unwrap_or(false),
                self.graph_path(),
            )
            .await,
        )
    }

    #[tool(description = "Create or update an agent-authored lesson for an atom.")]
    async fn upsert_lesson(
        &self,
        Parameters(args): Parameters<UpsertLessonArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let conn = self.conn().await?;
        let result = store::cmd_lesson_upsert(
            &conn,
            &args.atom,
            args.body,
            Some(&args.path_id),
            self.graph_path(),
        )
        .await;
        if result.is_ok() {
            self.spawn_sync();
        }
        encode(result.map(|()| json!({ "atom_id": args.atom })))
    }

    #[tool(description = "Create an agent-authored quiz.")]
    async fn create_quiz(
        &self,
        Parameters(args): Parameters<CreateQuizArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let conn = self.conn().await?;
        let result = store::cmd_quiz_create(
            &conn,
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
        if result.is_ok() {
            self.spawn_sync();
        }
        encode(result.map(|quiz_id| json!({ "quiz_id": quiz_id })))
    }

    #[tool(description = "Update fields of an existing quiz; ID and FSRS history are preserved.")]
    async fn update_quiz(
        &self,
        Parameters(args): Parameters<UpdateQuizArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let conn = self.conn().await?;
        let result = store::cmd_quiz_update(
            &conn,
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
        if result.is_ok() {
            self.spawn_sync();
        }
        encode(result.map(|()| json!({ "quiz_id": args.quiz_id })))
    }

    #[tool(description = "Tombstone a quiz so it no longer appears. Past answers stay in the log.")]
    async fn delete_quiz(
        &self,
        Parameters(args): Parameters<DeleteQuizArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let conn = self.conn().await?;
        let result =
            store::cmd_quiz_delete(&conn, &args.quiz_id, Some(&args.path_id), self.graph_path())
                .await;
        if result.is_ok() {
            self.spawn_sync();
        }
        encode(result.map(|()| json!({ "quiz_id": args.quiz_id })))
    }

    #[tool(description = "Record a user's quiz answer and FSRS rating.")]
    async fn answer_quiz(
        &self,
        Parameters(args): Parameters<AnswerQuizArgs>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let conn = self.conn().await?;
        let result = answer::cmd_quiz_answer(
            &conn,
            &args.quiz_id,
            args.rating,
            args.answer,
            Some(&args.path_id),
            self.graph_path(),
        )
        .await;
        if result.is_ok() {
            self.spawn_sync();
        }
        encode(result.map(|()| json!({ "quiz_id": args.quiz_id, "rating": args.rating })))
    }
}

// ───────────────────────────── prompts ────────────────────────────

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

// ───────────────────────────── handler ────────────────────────────

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

// ───────────────────── data helpers (shared by tools) ─────────────

#[derive(Serialize)]
struct PathSummary {
    id: String,
    goal: String,
    created_at: DateTime<Utc>,
    target_atoms: Vec<String>,
}

async fn list_paths(conn: &Connection) -> CrateResult<Vec<PathSummary>> {
    let mut rows = conn
        .query(
            "SELECT id, goal, created_at FROM paths ORDER BY created_at ASC",
            (),
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        let id: String = row.get(0)?;
        let goal: String = row.get(1)?;
        let created_str: String = row.get(2)?;
        let created_at = db::parse_ts(&created_str)?;
        let mut t_rows = conn
            .query(
                "SELECT atom_id FROM path_targets WHERE path_id = ? ORDER BY position ASC",
                libsql::params![id.as_str()],
            )
            .await?;
        let mut target_atoms = Vec::new();
        while let Some(r) = t_rows.next().await? {
            target_atoms.push(r.get::<String>(0)?);
        }
        out.push(PathSummary {
            id,
            goal,
            created_at,
            target_atoms,
        });
    }
    Ok(out)
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
    conn: &Connection,
    id: &str,
    path_id: Option<&str>,
    graph_dir: Option<&std::path::Path>,
) -> CrateResult<ItemView> {
    let (g, manifest) = load_graph_and_manifest(conn, path_id, graph_dir).await?;
    let item = discover::show_view(&g, &manifest, id)?;
    let status = if let Some(pid) = path_id {
        status_for(conn, &g, id, pid).await?
    } else {
        None
    };
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
    conn: &Connection,
    id: Option<&str>,
    path_id: Option<&str>,
    recursive: bool,
    graph_dir: Option<&std::path::Path>,
) -> CrateResult<ChildrenView> {
    let (g, manifest) = load_graph_and_manifest(conn, path_id, graph_dir).await?;
    let view = discover::list_view(&g, &manifest, id)?;
    let descendants = if recursive {
        let roots = match id {
            Some(s) => vec![s.to_string()],
            None => manifest.areas.iter().map(|a| a.prefix.clone()).collect(),
        };
        gather_descendants(conn, &g, &roots, path_id).await?
    } else {
        Vec::new()
    };
    Ok(ChildrenView { view, descendants })
}

async fn gather_descendants(
    conn: &Connection,
    g: &Graph,
    roots: &[String],
    path_id: Option<&str>,
) -> CrateResult<Vec<ChildStatus>> {
    let mut out = Vec::new();
    let mut stack: Vec<String> = roots.to_vec();
    while let Some(id) = stack.pop() {
        let Some(c) = g.by_id.get(&id) else { continue };
        for child in &c.children_ids {
            stack.push(child.clone());
        }
        let status = if let Some(pid) = path_id {
            status_for(conn, g, &id, pid).await?
        } else {
            None
        };
        out.push(ChildStatus {
            id: c.id.clone(),
            name: c.name.clone(),
            is_atom: c.children_ids.is_empty(),
            status,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

async fn status_for(
    conn: &Connection,
    g: &Graph,
    id: &str,
    path_id: &str,
) -> CrateResult<Option<AtomStatus>> {
    let Some(c) = g.by_id.get(id) else {
        return Ok(None);
    };
    if !c.children_ids.is_empty() {
        return Ok(None);
    }
    let progress = PathProgress::load(conn, path_id).await?;
    Ok(Some(AtomStatus {
        lesson_taught: progress.lesson_taught(id),
        complete: scheduler::is_atom_complete(g, &progress, id),
    }))
}

async fn load_graph_and_manifest(
    conn: &Connection,
    path_id: Option<&str>,
    graph_dir: Option<&std::path::Path>,
) -> CrateResult<(Graph, graph::Manifest)> {
    let g = if path_id.is_some() {
        Graph::load_for_path(conn, graph_dir).await?
    } else {
        Graph::load_default(graph_dir)?
    };
    let manifest = graph::load_manifest_default(graph_dir)?;
    Ok((g, manifest))
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
    conn: &Connection,
    path_id: &str,
    depth: Option<u32>,
    graph_dir: Option<&std::path::Path>,
) -> CrateResult<TreeView> {
    let p = path::load_path(conn, path_id).await?;
    let g = Graph::load_for_path(conn, graph_dir).await?;
    let manifest = graph::load_manifest_default(graph_dir)?;
    let progress = PathProgress::load(conn, path_id).await?;

    let reachable = tree::reachable_atoms(&g, &p.target_atoms);
    let spine = tree::build_spine(&g, &reachable);

    let targets_total = p.target_atoms.len();
    let targets_learned = p
        .target_atoms
        .iter()
        .filter(|a| scheduler::is_atom_complete(&g, &progress, a))
        .count();
    let targets_pct = if targets_total > 0 {
        targets_learned * 100 / targets_total
    } else {
        0
    };
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
        p.target_atoms.iter().map(String::as_str).collect();

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
        top.sort_by(|a, b| natural_cmp(&a.id, &b.id));
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
        kids.sort_by(|a, b| natural_cmp(&a.id, &b.id));
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

fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let ak: Vec<u32> = a.split('.').filter_map(|s| s.parse().ok()).collect();
    let bk: Vec<u32> = b.split('.').filter_map(|s| s.parse().ok()).collect();
    ak.cmp(&bk)
}

// ───────────────────────────── error mapping ──────────────────────

fn map_protocol_error(e: &Error) -> McpError {
    McpError::internal_error(format!("{e}"), None)
}

/// Map a fallible `crate::Result<T>` into a `CallToolResult`. Business
/// errors (atom not found, unknown id, invalid rating, etc.) become
/// `isError: true` with a structured JSON error body. Encoding-only
/// failures become protocol-level `McpError`.
///
/// MCP requires `structuredContent` to be a JSON object; bare arrays and
/// primitives get rejected by strict clients (Claude Desktop). Tools that
/// hold non-object data should wrap it (`{ "paths": [...] }`); this
/// helper still wraps anything that slips through as `{ "value": ... }`
/// so the failure mode is a self-describing payload, not a 500.
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
        Error::BadTimestamp(_) => "bad_timestamp",
        Error::UnknownEventKind(_) => "unknown_event_kind",
        Error::CardsCorrupt(_) => "cards_corrupt",
        Error::NoHome => "no_home",
        Error::FileIo { .. } | Error::AymlParse { .. } | Error::AymlSerialize(_) => "io",
        Error::Db(_) => "database",
        Error::Json(_) => "json",
        Error::Fsrs(_) => "fsrs",
        Error::MissingAuth
        | Error::BadBindAddr { .. }
        | Error::BadPublicUrl { .. }
        | Error::Bind { .. }
        | Error::Serve(_) => "server",
    }
}

// ───────────────────────────── HTTP entry point ───────────────────

/// Resolved auth configuration for `mt mcp`. At least one mode must be
/// enabled or the server refuses to start.
#[derive(Clone, Debug, Default)]
pub struct AuthConfig {
    /// Static-bearer mode. CLI / `mcp-remote` / test path; matches the
    /// raw token value. Set from `MT_API_KEY` or `--api-key`.
    pub api_key: Option<String>,
    /// OAuth-AS mode. Static password used by `/oauth/authorize`. Set
    /// from `MT_ADMIN_PASSWORD` or `--admin-password`.
    pub admin_password: Option<String>,
    /// Public-facing URL the server advertises in OAuth discovery
    /// metadata (issuer + `resource`). Must match what an external
    /// client types into "Add custom connector". Set from
    /// `MT_PUBLIC_URL` or `--public-url`; defaults to
    /// `http://<bind-addr>` when omitted.
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

/// Run the MCP server until SIGINT/SIGTERM. On shutdown, runs one final
/// foreground `db.sync()` so locally-acked writes land on Turso before
/// the process exits.
///
/// `addr` parses the same as `std::net::SocketAddr` (`"127.0.0.1:8080"`).
pub async fn run(
    addr: &str,
    auth: AuthConfig,
    cfg: DbConfig,
    graph_dir: Option<PathBuf>,
) -> CrateResult<()> {
    auth.validate()?;

    let db = Arc::new(db::open(&cfg).await?);
    let cfg_arc = Arc::new(cfg);

    // Pull the latest remote state before accepting traffic
    db::maybe_sync(&db, &cfg_arc).await;

    let shutdown = CancellationToken::new();
    let background = spawn_background_sync(db.clone(), cfg_arc.clone(), shutdown.child_token());

    let mcp_config = StreamableHttpServerConfig::default()
        .with_sse_keep_alive(Some(Duration::from_secs(15)))
        .with_cancellation_token(shutdown.child_token())
        .disable_allowed_hosts();

    let factory = {
        let db = db.clone();
        let cfg = cfg_arc.clone();
        let graph_dir = graph_dir.clone();
        move || {
            Ok(MathTutorServer::new(
                db.clone(),
                cfg.clone(),
                graph_dir.clone(),
            ))
        }
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
    // Auth middleware is scoped to MCP transport routes. Body logging
    // runs after auth so only authenticated payloads are buffered.
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

    // Drain the background sync task before the final sync so we don't
    // race with it on the same `Database` handle.
    background.await.ok();
    // Final sync — ensure every locally-acked write lands on Turso before
    // the process exits. Best-effort; `maybe_sync` already swallows errors.
    db::maybe_sync(&db, &cfg_arc).await;
    tracing::info!("mt mcp shutdown complete");
    Ok(())
}

/// Resolve the public-facing URL for discovery metadata. When the operator
/// hasn't set one explicitly we fall back to `http://<bind-addr>`; this is
/// only correct for local dev, but it lets `mt mcp` work zero-config.
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
    /// Database handle for OAuth access-token lookups. `None` when OAuth
    /// is disabled.
    db: Option<Arc<Database>>,
    /// Pre-formatted `WWW-Authenticate` value pointing at the protected-
    /// resource metadata, so OAuth-aware clients can discover the AS on
    /// 401s. Always populated — even bearer-only setups can advertise the
    /// resource ID, the lack of an authorization server just shows up as
    /// no `authorization_servers` entry in the metadata if the client
    /// fetches it (and bearer-only deployments shouldn't ship that route
    /// in the first place).
    www_authenticate: Arc<str>,
}

impl AuthState {
    /// Construct from the resolved `AuthConfig`. Stores only what the
    /// per-request middleware needs.
    fn new(auth: &AuthConfig, public_url: &Url, db: Arc<Database>) -> Self {
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

/// Layered bearer / OAuth auth on `/mcp`. Order:
///
/// 1. `Authorization: Bearer <token>` header.
/// 2. `?token=<token>` query parameter (legacy `EventSource` fallback,
///    bearer mode only — Claude clients don't use this path).
///
/// The presented value is matched against the static API key first
/// (when enabled), then looked up as an OAuth access token (when
/// enabled). On failure we return 401 with a `WWW-Authenticate` header
/// pointing at protected-resource metadata so OAuth-aware MCP clients
/// can discover the AS and trigger the connector flow.
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
        if let Some(db) = state.db.as_deref()
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
            // Body is consumed and can't be replayed — surface as 400 so
            // the client retries instead of seeing a confusing 500.
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

/// Liveness probe
async fn health() -> &'static str {
    "ok"
}

/// Wait for SIGINT or SIGTERM, whichever comes first. On non-unix
/// targets, only Ctrl-C is wired.
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

fn spawn_background_sync(
    db: Arc<Database>,
    cfg: Arc<DbConfig>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(BACKGROUND_SYNC_INTERVAL);
        // Drop the immediate first tick.
        // DB runs `maybe_sync` at startup so sync here is redundant.
        tick.tick().await;
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                _ = tick.tick() => db::maybe_sync(&db, &cfg).await,
            }
        }
    })
}
