use std::path::PathBuf;
use std::process::ExitCode;

use argh::FromArgs;

mod graph;

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
    }
}
