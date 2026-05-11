use std::process::ExitCode;

use mathtutor::cli::{Cmd, GraphOp, Mt, OverlayOp, StoreOp};
use mathtutor::{answer, discover, graph, overlay, path, scheduler, state, store, tree};

fn run_simple<E: std::fmt::Display>(
    result: Result<(), E>,
    ok_code: ExitCode,
    err_code: u8,
) -> ExitCode {
    match result {
        Ok(()) => ok_code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(err_code)
        }
    }
}

#[allow(clippy::too_many_lines)]
fn main() -> ExitCode {
    let cli: Mt = argh::from_env();
    match cli.cmd {
        Cmd::Graph(g) => match g.op {
            GraphOp::Check(c) => match graph::run_check(c.path.as_deref()) {
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
        Cmd::New(c) => match path::cmd_new(&c.goal, &c.atom, c.graph.as_deref()) {
            Ok(id) => {
                eprintln!("created path: {id}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(2)
            }
        },
        Cmd::Next(c) => match scheduler::cmd_next(c.path.as_deref(), c.graph.as_deref()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(1)
            }
        },
        Cmd::State(c) => match state::cmd_state(c.path.as_deref(), c.graph.as_deref()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(1)
            }
        },
        Cmd::Tree(c) => match tree::cmd_tree(c.path.as_deref(), c.graph.as_deref()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(1)
            }
        },
        Cmd::Store(s) => match s.op {
            StoreOp::Lesson(c) => match store::cmd_store_lesson(
                &c.atom,
                c.body,
                c.path.as_deref(),
                c.graph.as_deref(),
            ) {
                Ok(()) => {
                    eprintln!("stored lesson: {}", c.atom);
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::from(2)
                }
            },
            StoreOp::Quiz(c) => match store::cmd_store_quiz(
                &c.atom,
                c.difficulty,
                c.question,
                c.answer,
                c.rubric,
                c.quiz_type,
                c.path.as_deref(),
                c.graph.as_deref(),
            ) {
                Ok(qid) => {
                    eprintln!("stored quiz: {qid}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::from(2)
                }
            },
        },
        Cmd::Answer(c) => {
            match answer::cmd_answer(&c.quiz, c.rating, c.user_answer, c.path.as_deref()) {
                Ok(()) => {
                    eprintln!("recorded {} for {}", c.rating, c.quiz);
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::from(2)
                }
            }
        }
        Cmd::Show(c) => run_simple(
            discover::cmd_show(&c.id, c.graph.as_deref()),
            ExitCode::SUCCESS,
            2,
        ),
        Cmd::List(c) => run_simple(
            discover::cmd_list(c.id.as_deref(), c.graph.as_deref()),
            ExitCode::SUCCESS,
            2,
        ),
        Cmd::Overlay(o) => match o.op {
            OverlayOp::Dump(c) => {
                run_simple(overlay::cmd_dump(c.path.as_deref()), ExitCode::SUCCESS, 1)
            }
        },
    }
}
