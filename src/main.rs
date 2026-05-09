use std::path::PathBuf;
use std::process::ExitCode;

use argh::FromArgs;

mod event_log;
mod graph;
mod path;
mod persist;
mod scheduler;

#[derive(FromArgs, Debug)]
/// Math Tutor — small lessons + spaced repetition over a curriculum graph.
struct Mt {
    #[argh(subcommand)]
    cmd: Cmd,
}

#[derive(FromArgs, Debug)]
#[argh(subcommand)]
enum Cmd {
    Graph(GraphCmd),
    New(NewCmd),
    Next(NextCmd),
    State(StateCmd),
    Store(StoreCmd),
}

/// Operate on the curriculum graph.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "graph")]
struct GraphCmd {
    #[argh(subcommand)]
    op: GraphOp,
}

#[derive(FromArgs, Debug)]
#[argh(subcommand)]
enum GraphOp {
    Check(GraphCheck),
}

/// Validate the curriculum graph.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "check")]
struct GraphCheck {
    /// path to the graph directory (default: curriculum/graph)
    #[argh(option, short = 'p', default = "PathBuf::from(\"curriculum/graph\")")]
    path: PathBuf,
}

/// Start a new learning path.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "new")]
struct NewCmd {
    /// the user's goal in plain text
    #[argh(positional)]
    goal: String,

    /// target atom ID (repeatable)
    #[argh(option)]
    atom: Vec<String>,

    /// path to the curriculum graph directory (default: curriculum/graph)
    #[argh(option, default = "PathBuf::from(\"curriculum/graph\")")]
    graph: PathBuf,
}

/// Get the next action for a learning path.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "next")]
struct NextCmd {
    /// path id (defaults to most recent)
    #[argh(option, short = 'p')]
    path: Option<String>,

    /// path to the curriculum graph directory (default: curriculum/graph)
    #[argh(option, default = "PathBuf::from(\"curriculum/graph\")")]
    graph: PathBuf,
}

/// Show the state of a learning path.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "state")]
struct StateCmd {
    /// path id (defaults to most recent)
    #[argh(option, short = 'p')]
    path: Option<String>,
}

/// Store agent-authored content into the canonical curriculum graph.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "store")]
struct StoreCmd {
    #[argh(subcommand)]
    op: StoreOp,
}

#[derive(FromArgs, Debug)]
#[argh(subcommand)]
enum StoreOp {
    Lesson(StoreLessonCmd),
}

/// Persist a lesson body on an atom.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "lesson")]
struct StoreLessonCmd {
    /// atom id
    #[argh(positional)]
    atom: String,

    /// lesson body
    #[argh(option)]
    body: String,

    /// path id (defaults to most recent)
    #[argh(option, short = 'p')]
    path: Option<String>,

    /// path to the curriculum graph directory (default: curriculum/graph)
    #[argh(option, default = "PathBuf::from(\"curriculum/graph\")")]
    graph: PathBuf,
}

fn main() -> ExitCode {
    let cli: Mt = argh::from_env();
    match cli.cmd {
        Cmd::Graph(g) => match g.op {
            GraphOp::Check(c) => match graph::run_check(&c.path) {
                Ok(report) => {
                    report.print();
                    if report.issues.is_empty() {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::from(1)
                    }
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::from(2)
                }
            },
        },
        Cmd::New(c) => match path::cmd_new(&c.goal, &c.atom, &c.graph) {
            Ok(id) => {
                eprintln!("created path: {id}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(2)
            }
        },
        Cmd::Next(c) => match scheduler::cmd_next(c.path.as_deref(), &c.graph) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(1)
            }
        },
        Cmd::State(c) => match path::cmd_state(c.path.as_deref()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(1)
            }
        },
        Cmd::Store(s) => match s.op {
            StoreOp::Lesson(c) => {
                match persist::cmd_store_lesson(
                    &c.atom,
                    c.body.clone(),
                    c.path.as_deref(),
                    &c.graph,
                ) {
                    Ok(()) => {
                        eprintln!("stored lesson: {}", c.atom);
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        ExitCode::from(2)
                    }
                }
            }
        },
    }
}
