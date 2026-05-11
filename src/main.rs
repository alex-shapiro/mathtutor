use std::process::ExitCode;

use mathtutor::cli::{Cmd, GraphOp, Mt, OverlayOp, StoreOp};
use mathtutor::{
    Result, answer, discover, graph, instruct, overlay, path, scheduler, state, store, tree,
};

fn main() -> ExitCode {
    let cli: Mt = argh::from_env();

    // `mt graph check` has its own success-vs-issues exit logic and
    // prints its report independently — handle outside the unified
    // dispatch.
    if let Cmd::Graph(g) = &cli.cmd {
        let GraphOp::Check(c) = &g.op;
        return match graph::run_check(c.path.as_deref()) {
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
        };
    }

    let (result, err_code) = dispatch(cli.cmd);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(err_code)
        }
    }
}

/// Run the chosen subcommand. Returns the result plus the exit code to
/// use on failure — most commands exit `2` (config / IO / validation),
/// `mt next` / `mt state` / `mt tree` / `mt overlay dump` exit `1` to
/// distinguish "scheduling / state read failure" from "you held it wrong."
fn dispatch(cmd: Cmd) -> (Result<()>, u8) {
    match cmd {
        Cmd::Graph(_) => unreachable!("Graph handled in main"),
        Cmd::New(c) => {
            let r = path::cmd_new(&c.goal, &c.atom, c.graph.as_deref()).map(|id| {
                eprintln!("created path: {id}");
            });
            (r, 2)
        }
        Cmd::Next(c) => (
            scheduler::cmd_next(c.path.as_deref(), c.graph.as_deref()),
            1,
        ),
        Cmd::State(c) => (state::cmd_state(c.path.as_deref(), c.graph.as_deref()), 1),
        Cmd::Tree(c) => (tree::cmd_tree(c.path.as_deref(), c.graph.as_deref()), 1),
        Cmd::Store(s) => match s.op {
            StoreOp::Lesson(c) => {
                let atom = c.atom.clone();
                let r =
                    store::cmd_store_lesson(&c.atom, c.body, c.path.as_deref(), c.graph.as_deref())
                        .map(|()| {
                            eprintln!("stored lesson: {atom}");
                        });
                (r, 2)
            }
            StoreOp::Quiz(c) => {
                let r = store::cmd_store_quiz(
                    &c.atom,
                    c.difficulty,
                    c.question,
                    c.answer,
                    c.rubric,
                    c.quiz_type,
                    c.path.as_deref(),
                    c.graph.as_deref(),
                )
                .map(|qid| {
                    eprintln!("stored quiz: {qid}");
                });
                (r, 2)
            }
        },
        Cmd::Answer(c) => {
            let quiz = c.quiz.clone();
            let rating = c.rating;
            let r =
                answer::cmd_answer(&c.quiz, c.rating, c.user_answer, c.path.as_deref()).map(|()| {
                    eprintln!("recorded {rating} for {quiz}");
                });
            (r, 2)
        }
        Cmd::Show(c) => (discover::cmd_show(&c.id, c.graph.as_deref()), 2),
        Cmd::List(c) => (discover::cmd_list(c.id.as_deref(), c.graph.as_deref()), 2),
        Cmd::Overlay(o) => match o.op {
            OverlayOp::Dump(c) => (overlay::cmd_dump(c.path.as_deref()), 1),
        },
        Cmd::Instruct(_) => (instruct::cmd_instruct(), 1),
    }
}
