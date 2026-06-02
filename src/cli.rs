//! CLI surface — every `argh::FromArgs` struct lives here. main.rs
//! imports the top-level `Mt` parser and dispatches on its variants.
//!
//! Subcommand layout is resource-first (`mt <noun> <verb>`) and tracks
//! the MCP tool surface 1:1. Operator-only verbs (`graph check`,
//! `graph dump`, `instruct`, `migrate-from-ayml`, `mcp`) have no MCP
//! equivalent and are explicitly flagged in the docs.
//!
//! Curriculum location: the binary ships an embedded copy of the
//! curriculum graph (see `graph::EMBEDDED_GRAPH`). The `--graph DIR`
//! flag and `MT_GRAPH` environment variable both override this for
//! development against a working tree. Per-command, `graph` is an
//! `Option<PathBuf>` whose absence means "use embedded / env".

use std::path::PathBuf;

use argh::FromArgs;

use crate::types;

#[derive(FromArgs, Debug)]
/// Math Tutor — small lessons + spaced repetition over a curriculum graph.
pub struct Mt {
    #[argh(subcommand)]
    pub cmd: Cmd,
}

#[derive(FromArgs, Debug)]
#[argh(subcommand)]
pub enum Cmd {
    Path(PathCmd),
    Graph(GraphCmd),
    Lesson(LessonCmd),
    Quiz(QuizCmd),
    Instruct(InstructCmd),
    MigrateFromAyml(MigrateFromAymlCmd),
    #[cfg(feature = "mcp")]
    Mcp(McpCmd),
}

// ── `mt path …` ────────────────────────────────────────────────────

/// Operate on learning paths.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "path")]
pub struct PathCmd {
    #[argh(subcommand)]
    pub op: PathOp,
}

#[derive(FromArgs, Debug)]
#[argh(subcommand)]
pub enum PathOp {
    List(PathListCmd),
    New(PathNewCmd),
    State(PathStateCmd),
    Next(PathNextCmd),
    Syllabus(PathSyllabusCmd),
    Tree(PathTreeCmd),
}

/// List every learning path with goal, creation time, and progress.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "list")]
pub struct PathListCmd {
    /// override path to a curriculum graph directory (default: embedded / `$MT_GRAPH`)
    #[argh(option)]
    pub graph: Option<PathBuf>,
}

/// Start a new learning path.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "new")]
pub struct PathNewCmd {
    /// the user's goal in plain text
    #[argh(positional)]
    pub goal: String,

    /// target atom ID (repeatable)
    #[argh(option)]
    pub atom: Vec<String>,

    /// override path to a curriculum graph directory (default: embedded / `$MT_GRAPH`)
    #[argh(option)]
    pub graph: Option<PathBuf>,
}

/// Show the state of a learning path.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "state")]
pub struct PathStateCmd {
    /// path id (defaults to most recent)
    #[argh(option, short = 'p')]
    pub path: Option<String>,

    /// override path to a curriculum graph directory (default: embedded / `$MT_GRAPH`)
    #[argh(option)]
    pub graph: Option<PathBuf>,
}

/// Get the next action for a learning path.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "next")]
pub struct PathNextCmd {
    /// path id (defaults to most recent)
    #[argh(option, short = 'p')]
    pub path: Option<String>,

    /// override path to a curriculum graph directory (default: embedded / `$MT_GRAPH`)
    #[argh(option)]
    pub graph: Option<PathBuf>,
}

/// Preview the next upcoming lesson topics for a path. Forward-looking
/// only — atoms whose lesson is already taught (and any in-progress
/// quiz work on them) are skipped. Lesson bodies are not included.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "syllabus")]
pub struct PathSyllabusCmd {
    /// path id (defaults to most recent)
    #[argh(option, short = 'p')]
    pub path: Option<String>,

    /// max upcoming atoms to return (default: 10)
    #[argh(option, short = 'n', default = "10")]
    pub n: usize,

    /// override path to a curriculum graph directory (default: embedded / `$MT_GRAPH`)
    #[argh(option)]
    pub graph: Option<PathBuf>,
}

/// Show the path's full prerequisite tree with per-atom progress.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "tree")]
pub struct PathTreeCmd {
    /// path id (defaults to most recent)
    #[argh(option, short = 'p')]
    pub path: Option<String>,

    /// override path to a curriculum graph directory (default: embedded / `$MT_GRAPH`)
    #[argh(option)]
    pub graph: Option<PathBuf>,
}

// ── `mt graph …` ───────────────────────────────────────────────────

/// Inspect or audit the curriculum graph.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "graph")]
pub struct GraphCmd {
    #[argh(subcommand)]
    pub op: GraphOp,
}

#[derive(FromArgs, Debug)]
#[argh(subcommand)]
pub enum GraphOp {
    Show(GraphShowCmd),
    List(GraphListCmd),
    Check(GraphCheckCmd),
    Dump(GraphDumpCmd),
}

/// Look up a single curriculum entry (atom, cluster, or area).
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "show")]
pub struct GraphShowCmd {
    /// id to show (atom, cluster, or area prefix)
    #[argh(positional)]
    pub id: String,

    /// path id (when set, atom output is enriched with per-path status)
    #[argh(option, short = 'p')]
    pub path: Option<String>,

    /// override path to a curriculum graph directory (default: embedded / `$MT_GRAPH`)
    #[argh(option)]
    pub graph: Option<PathBuf>,
}

/// List entries in the curriculum.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "list")]
pub struct GraphListCmd {
    /// id to list children of (omit for all areas)
    #[argh(positional)]
    pub id: Option<String>,

    /// path id (when set, atom children are enriched with per-path status)
    #[argh(option, short = 'p')]
    pub path: Option<String>,

    /// override path to a curriculum graph directory (default: embedded / `$MT_GRAPH`)
    #[argh(option)]
    pub graph: Option<PathBuf>,
}

/// Validate the curriculum graph (operator-only — no MCP equivalent).
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "check")]
pub struct GraphCheckCmd {
    /// override path to a graph directory (default: embedded / `$MT_GRAPH`)
    #[argh(option, short = 'p')]
    pub path: Option<PathBuf>,
}

/// Print the user overlay to stdout, for review or upstreaming
/// (operator-only — no MCP equivalent).
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "dump")]
pub struct GraphDumpCmd {}

// ── `mt lesson …` ──────────────────────────────────────────────────

/// Operate on lessons in the user overlay.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "lesson")]
pub struct LessonCmd {
    #[argh(subcommand)]
    pub op: LessonOp,
}

#[derive(FromArgs, Debug)]
#[argh(subcommand)]
pub enum LessonOp {
    Upsert(LessonUpsertCmd),
}

/// Upsert a lesson body on an atom. Re-running replaces the body.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "upsert")]
pub struct LessonUpsertCmd {
    /// atom id
    #[argh(positional)]
    pub atom: String,

    /// lesson body
    #[argh(option)]
    pub body: String,

    /// path id (defaults to most recent)
    #[argh(option, short = 'p')]
    pub path: Option<String>,

    /// override path to a curriculum graph directory (default: embedded / `$MT_GRAPH`)
    #[argh(option)]
    pub graph: Option<PathBuf>,
}

// ── `mt quiz …` ────────────────────────────────────────────────────

/// Operate on quizzes in the user overlay.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "quiz")]
pub struct QuizCmd {
    #[argh(subcommand)]
    pub op: QuizOp,
}

#[derive(FromArgs, Debug)]
#[argh(subcommand)]
pub enum QuizOp {
    Create(QuizCreateCmd),
    Update(QuizUpdateCmd),
    Delete(QuizDeleteCmd),
    Answer(QuizAnswerCmd),
}

/// Create a quiz on an atom (free-text by default).
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "create")]
pub struct QuizCreateCmd {
    /// atom id
    #[argh(positional)]
    pub atom: String,

    /// difficulty: easy | medium | hard
    #[argh(option)]
    pub difficulty: types::Difficulty,

    /// question text
    #[argh(option)]
    pub question: String,

    /// reference answer
    #[argh(option)]
    pub answer: String,

    /// optional grading rubric
    #[argh(option)]
    pub rubric: Option<String>,

    /// quiz type: `free_text` (default) | `multiple_choice`
    #[argh(option, long = "type", default = "types::QuizType::FreeText")]
    pub quiz_type: types::QuizType,

    /// path id (defaults to most recent)
    #[argh(option, short = 'p')]
    pub path: Option<String>,

    /// override path to a curriculum graph directory (default: embedded / `$MT_GRAPH`)
    #[argh(option)]
    pub graph: Option<PathBuf>,
}

/// Apply field edits to an existing quiz. Only the supplied fields
/// change; the quiz id and FSRS history are preserved.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "update")]
pub struct QuizUpdateCmd {
    /// quiz id (e.g. fnd.1.1.1.q1)
    #[argh(positional)]
    pub quiz: String,

    /// new question text
    #[argh(option)]
    pub question: Option<String>,

    /// new reference answer
    #[argh(option)]
    pub answer: Option<String>,

    /// new grading rubric
    #[argh(option)]
    pub rubric: Option<String>,

    /// new difficulty: easy | medium | hard
    #[argh(option)]
    pub difficulty: Option<types::Difficulty>,

    /// new quiz type: `free_text` | `multiple_choice`
    #[argh(option, long = "type")]
    pub quiz_type: Option<types::QuizType>,

    /// path id (defaults to most recent)
    #[argh(option, short = 'p')]
    pub path: Option<String>,

    /// override path to a curriculum graph directory (default: embedded / `$MT_GRAPH`)
    #[argh(option)]
    pub graph: Option<PathBuf>,
}

/// Tombstone a quiz so it no longer appears in the merged view for
/// this path. The quiz's `quiz_answered` events stay in the log for
/// audit; the scheduler just stops surfacing it.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "delete")]
pub struct QuizDeleteCmd {
    /// quiz id (e.g. fnd.1.1.1.q1)
    #[argh(positional)]
    pub quiz: String,

    /// path id (defaults to most recent)
    #[argh(option, short = 'p')]
    pub path: Option<String>,

    /// override path to a curriculum graph directory (default: embedded / `$MT_GRAPH`)
    #[argh(option)]
    pub graph: Option<PathBuf>,
}

/// Record a quiz answer as an FSRS rating.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "answer")]
pub struct QuizAnswerCmd {
    /// quiz id (e.g. fnd.1.1.1.q1)
    #[argh(positional)]
    pub quiz: String,

    /// rating: again | hard | good | easy
    #[argh(option)]
    pub rating: types::Rating,

    /// the user's reply, verbatim — logged with the rating for review
    #[argh(option, long = "user-answer")]
    pub user_answer: Option<String>,

    /// path id (defaults to most recent)
    #[argh(option, short = 'p')]
    pub path: Option<String>,

    /// override path to a curriculum graph directory (default: embedded / `$MT_GRAPH`)
    #[argh(option)]
    pub graph: Option<PathBuf>,
}

// ── Operator-only commands ─────────────────────────────────────────

/// Print the agent operator playbook embedded in the binary.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "instruct")]
pub struct InstructCmd {}

/// Port legacy AYML state under `$MATHTUTOR_HOME/paths/` into the
/// libSQL database. Idempotent: re-running skips paths and overlay
/// rows that are already present.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "migrate-from-ayml")]
pub struct MigrateFromAymlCmd {
    /// override the AYML root (default: `$MATHTUTOR_HOME` / `~/.mathtutor`)
    #[argh(option)]
    pub from: Option<PathBuf>,
}

/// Run the MCP server (SSE over HTTP).
///
/// Authentication is layered. Set at least one of:
///
/// * `--api-key` / `$MT_API_KEY` — static bearer token (CLI / `mcp-remote` /
///   test path).
/// * `--admin-password` / `$MT_ADMIN_PASSWORD` — admin password for the
///   built-in OAuth authorization server (Claude Desktop / iOS path).
#[cfg(feature = "mcp")]
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "mcp")]
pub struct McpCmd {
    /// bind address (default: 127.0.0.1:8080)
    #[argh(option, default = "default_mcp_addr()")]
    pub addr: String,

    /// shared API key for static-bearer auth (overrides `$MT_API_KEY`)
    #[argh(option)]
    pub api_key: Option<String>,

    /// admin password for the built-in OAuth flow (overrides `$MT_ADMIN_PASSWORD`)
    #[argh(option)]
    pub admin_password: Option<String>,

    /// public-facing base URL advertised in OAuth discovery metadata
    /// (overrides `$MT_PUBLIC_URL`; defaults to `http://<addr>`)
    #[argh(option)]
    pub public_url: Option<String>,

    /// override path to a curriculum graph directory (default: embedded / `$MT_GRAPH`)
    #[argh(option)]
    pub graph: Option<PathBuf>,
}

#[cfg(feature = "mcp")]
fn default_mcp_addr() -> String {
    "127.0.0.1:8080".into()
}
