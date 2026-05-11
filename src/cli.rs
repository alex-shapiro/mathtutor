//! CLI surface — every `argh::FromArgs` struct lives here. main.rs
//! imports the top-level `Mt` parser and dispatches on its variants.
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
    New(NewCmd),
    State(StateCmd),
    Next(NextCmd),
    Store(StoreCmd),
    Answer(AnswerCmd),
    Show(ShowCmd),
    List(ListCmd),
    Tree(TreeCmd),
    Overlay(OverlayCmd),
    Graph(GraphCmd),
}

/// Look up a single curriculum entry (atom, cluster, or area).
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "show")]
pub struct ShowCmd {
    /// id to show (atom, cluster, or area prefix)
    #[argh(positional)]
    pub id: String,

    /// override path to a curriculum graph directory (default: embedded / `$MT_GRAPH`)
    #[argh(option)]
    pub graph: Option<PathBuf>,
}

/// List entries in the curriculum.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "list")]
pub struct ListCmd {
    /// id to list children of (omit for all areas)
    #[argh(positional)]
    pub id: Option<String>,

    /// override path to a curriculum graph directory (default: embedded / `$MT_GRAPH`)
    #[argh(option)]
    pub graph: Option<PathBuf>,
}

/// Operate on the curriculum graph.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "graph")]
pub struct GraphCmd {
    #[argh(subcommand)]
    pub op: GraphOp,
}

#[derive(FromArgs, Debug)]
#[argh(subcommand)]
pub enum GraphOp {
    Check(GraphCheck),
}

/// Validate the curriculum graph.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "check")]
pub struct GraphCheck {
    /// override path to a graph directory (default: embedded / `$MT_GRAPH`)
    #[argh(option, short = 'p')]
    pub path: Option<PathBuf>,
}

/// Start a new learning path.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "new")]
pub struct NewCmd {
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

/// Get the next action for a learning path.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "next")]
pub struct NextCmd {
    /// path id (defaults to most recent)
    #[argh(option, short = 'p')]
    pub path: Option<String>,

    /// override path to a curriculum graph directory (default: embedded / `$MT_GRAPH`)
    #[argh(option)]
    pub graph: Option<PathBuf>,
}

/// Show the state of a learning path.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "state")]
pub struct StateCmd {
    /// path id (defaults to most recent)
    #[argh(option, short = 'p')]
    pub path: Option<String>,

    /// override path to a curriculum graph directory (default: embedded / `$MT_GRAPH`)
    #[argh(option)]
    pub graph: Option<PathBuf>,
}

/// Show the path's full prerequisite tree with per-atom progress.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "tree")]
pub struct TreeCmd {
    /// path id (defaults to most recent)
    #[argh(option, short = 'p')]
    pub path: Option<String>,

    /// override path to a curriculum graph directory (default: embedded / `$MT_GRAPH`)
    #[argh(option)]
    pub graph: Option<PathBuf>,
}

/// Store agent-authored content into the active learning path's overlay.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "store")]
pub struct StoreCmd {
    #[argh(subcommand)]
    pub op: StoreOp,
}

#[derive(FromArgs, Debug)]
#[argh(subcommand)]
pub enum StoreOp {
    Lesson(StoreLessonCmd),
    Quiz(StoreQuizCmd),
}

/// Persist a lesson body on an atom.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "lesson")]
pub struct StoreLessonCmd {
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

/// Persist a quiz on an atom (free-text by default).
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "quiz")]
pub struct StoreQuizCmd {
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

/// Record a quiz answer as an FSRS rating.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "answer")]
pub struct AnswerCmd {
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
}

/// Operate on a learning path's user-authored overlay.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "overlay")]
pub struct OverlayCmd {
    #[argh(subcommand)]
    pub op: OverlayOp,
}

#[derive(FromArgs, Debug)]
#[argh(subcommand)]
pub enum OverlayOp {
    Dump(OverlayDumpCmd),
}

/// Print the active path's overlay to stdout, for review or upstreaming.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "dump")]
pub struct OverlayDumpCmd {
    /// path id (defaults to most recent)
    #[argh(option, short = 'p')]
    pub path: Option<String>,
}
