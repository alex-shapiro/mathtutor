//! CLI surface — every `argh::FromArgs` struct lives here. main.rs
//! imports the top-level `Mt` parser and dispatches on its variants.

use std::path::PathBuf;

use argh::FromArgs;

use crate::types;

/// Curriculum-graph location for `--graph` / `-p` defaults. `MT_GRAPH`
/// lets the agent run `mt` from any cwd; falling back to the
/// project-relative path matches in-tree development.
fn default_graph_dir() -> PathBuf {
    std::env::var_os("MT_GRAPH").map_or_else(|| PathBuf::from("curriculum/graph"), PathBuf::from)
}

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
    Graph(GraphCmd),
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
    /// path to the graph directory (default: `$MT_GRAPH` or `curriculum/graph`)
    #[argh(option, short = 'p', default = "default_graph_dir()")]
    pub path: PathBuf,
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

    /// path to the curriculum graph directory (default: curriculum/graph)
    #[argh(option, default = "default_graph_dir()")]
    pub graph: PathBuf,
}

/// Get the next action for a learning path.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "next")]
pub struct NextCmd {
    /// path id (defaults to most recent)
    #[argh(option, short = 'p')]
    pub path: Option<String>,

    /// path to the curriculum graph directory (default: curriculum/graph)
    #[argh(option, default = "default_graph_dir()")]
    pub graph: PathBuf,
}

/// Show the state of a learning path.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "state")]
pub struct StateCmd {
    /// path id (defaults to most recent)
    #[argh(option, short = 'p')]
    pub path: Option<String>,

    /// path to the curriculum graph directory (default: `$MT_GRAPH` or `curriculum/graph`)
    #[argh(option, default = "default_graph_dir()")]
    pub graph: PathBuf,
}

/// Store agent-authored content into the canonical curriculum graph.
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

    /// path to the curriculum graph directory (default: curriculum/graph)
    #[argh(option, default = "default_graph_dir()")]
    pub graph: PathBuf,
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

    /// path to the curriculum graph directory (default: curriculum/graph)
    #[argh(option, default = "default_graph_dir()")]
    pub graph: PathBuf,
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

    /// path id (defaults to most recent)
    #[argh(option, short = 'p')]
    pub path: Option<String>,

    /// path to the curriculum graph directory (default: curriculum/graph)
    #[argh(option, default = "default_graph_dir()")]
    pub graph: PathBuf,
}
