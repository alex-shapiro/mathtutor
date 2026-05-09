use std::process::ExitCode;

use mathtutor::cli::{Cmd, GraphOp, Mt, StoreOp};
use mathtutor::{answer, discover, graph, path, scheduler, state, store};

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
        Cmd::State(c) => match state::cmd_state(c.path.as_deref(), &c.graph) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(1)
            }
        },
        Cmd::Store(s) => match s.op {
            StoreOp::Lesson(c) => {
                match store::cmd_store_lesson(&c.atom, c.body, c.path.as_deref(), &c.graph) {
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
            StoreOp::Quiz(c) => match store::cmd_store_quiz(
                &c.atom,
                c.difficulty,
                c.question,
                c.answer,
                c.rubric,
                c.quiz_type,
                c.path.as_deref(),
                &c.graph,
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
            match answer::cmd_answer(&c.quiz, c.rating, c.path.as_deref(), &c.graph) {
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
        Cmd::Show(c) => run_simple(discover::cmd_show(&c.id, &c.graph), ExitCode::SUCCESS, 2),
        Cmd::List(c) => run_simple(
            discover::cmd_list(c.id.as_deref(), &c.graph),
            ExitCode::SUCCESS,
            2,
        ),
    }
}
