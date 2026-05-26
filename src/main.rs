use std::process::ExitCode;

use libsql::Connection;
use mathtutor::cli::{AmendOp, Cmd, GraphOp, Mt, OverlayOp, RemoveOp, StoreOp};
#[cfg(feature = "mcp")]
use mathtutor::mcp;
use mathtutor::{
    Result, answer, db, discover, graph, instruct, migrate, overlay, path, scheduler, state, store,
    tree,
};

#[cfg(feature = "mcp")]
#[tokio::main]
async fn main() -> ExitCode {
    real_main().await
}

#[cfg(not(feature = "mcp"))]
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    real_main().await
}

async fn real_main() -> ExitCode {
    let cli: Mt = argh::from_env();

    init_tracing(is_mcp(&cli.cmd));

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

    // `mt instruct` is read-only and doesn't touch the database.
    if matches!(cli.cmd, Cmd::Instruct(_)) {
        return match instruct::cmd_instruct() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(1)
            }
        };
    }

    // `mt show` / `mt list` operate purely on the embedded curriculum
    // graph and have no per-user state — skip opening the database.
    if let Cmd::Show(c) = &cli.cmd {
        return run_simple(discover::cmd_show(&c.id, c.graph.as_deref()), 2);
    }
    if let Cmd::List(c) = &cli.cmd {
        return run_simple(discover::cmd_list(c.id.as_deref(), c.graph.as_deref()), 2);
    }

    // `mt mcp` owns its own DB lifecycle (long-running, background sync
    // task, graceful shutdown) so it sits outside the per-command DB
    // setup the rest of the dispatch block does.
    #[cfg(feature = "mcp")]
    if let Cmd::Mcp(c) = cli.cmd {
        let auth = mcp::AuthConfig {
            api_key: nonempty(c.api_key.or_else(|| std::env::var("MT_API_KEY").ok())),
            admin_password: nonempty(
                c.admin_password
                    .or_else(|| std::env::var("MT_ADMIN_PASSWORD").ok()),
            ),
            public_url: nonempty(c.public_url.or_else(|| std::env::var("MT_PUBLIC_URL").ok())),
        };
        let cfg = match db::DbConfig::from_env() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(2);
            }
        };
        return match mcp::run(&c.addr, auth, cfg, c.graph).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(2)
            }
        };
    }

    let cfg = match db::DbConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let db = match db::open(&cfg).await {
        Ok(db) => db,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let conn = match db::connect(&db).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };

    let modifies_state = mutating(&cli.cmd);
    let (result, err_code) = dispatch(&conn, cli.cmd).await;
    // Push to the Turso replica after a successful state change.
    // Failure is non-fatal; libSQL catches up later.
    if modifies_state && result.is_ok() {
        db::maybe_sync(&db, &cfg).await;
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(err_code)
        }
    }
}

/// Commands that write to the database. Read-only commands (`state`,
/// `tree`, `overlay dump`) don't trigger a foreground sync; pure
/// curriculum lookups (`show`, `list`) don't even open the database.
fn mutating(cmd: &Cmd) -> bool {
    matches!(
        cmd,
        Cmd::New(_)
            | Cmd::Next(_)
            | Cmd::Answer(_)
            | Cmd::Store(_)
            | Cmd::Amend(_)
            | Cmd::Remove(_)
            | Cmd::MigrateFromAyml(_)
    )
}

/// CLI helper: treat empty strings (e.g. `MT_API_KEY=`) as "not set" so
/// the env-var fallback doesn't accidentally feed an empty token into
/// the constant-time bearer compare.
#[cfg(feature = "mcp")]
fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.is_empty())
}

#[cfg(feature = "mcp")]
fn is_mcp(cmd: &Cmd) -> bool {
    matches!(cmd, Cmd::Mcp(_))
}

#[cfg(not(feature = "mcp"))]
fn is_mcp(_cmd: &Cmd) -> bool {
    false
}

/// Install the global `tracing` subscriber. CLI commands run quietly
/// (only `warn`+); `mt mcp` opts into `info`-level logs from our crate,
/// `rmcp`, and `tower_http` so request-level events from the long-
/// running server show up by default. `RUST_LOG` overrides either side.
fn init_tracing(verbose: bool) {
    let default_filter = if verbose {
        "mathtutor=info,rmcp=info,tower_http=info,warn"
    } else {
        "warn"
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| default_filter.into());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();
}

fn run_simple(result: Result<()>, err_code: u8) -> ExitCode {
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
#[allow(clippy::too_many_lines)]
async fn dispatch(conn: &Connection, cmd: Cmd) -> (Result<()>, u8) {
    match cmd {
        Cmd::Graph(_) | Cmd::Instruct(_) | Cmd::Show(_) | Cmd::List(_) => {
            unreachable!("handled before dispatch")
        }
        #[cfg(feature = "mcp")]
        Cmd::Mcp(_) => unreachable!("handled before dispatch"),
        Cmd::New(c) => {
            let r = path::cmd_new(conn, &c.goal, &c.atom, c.graph.as_deref())
                .await
                .map(|id| {
                    eprintln!("created path: {id}");
                });
            (r, 2)
        }
        Cmd::Next(c) => (
            scheduler::cmd_next(conn, c.path.as_deref(), c.graph.as_deref()).await,
            1,
        ),
        Cmd::State(c) => (
            state::cmd_state(conn, c.path.as_deref(), c.graph.as_deref()).await,
            1,
        ),
        Cmd::Tree(c) => (
            tree::cmd_tree(conn, c.path.as_deref(), c.graph.as_deref()).await,
            1,
        ),
        Cmd::Store(s) => match s.op {
            StoreOp::Lesson(c) => {
                let atom = c.atom.clone();
                let r = store::cmd_store_lesson(
                    conn,
                    &c.atom,
                    c.body,
                    c.path.as_deref(),
                    c.graph.as_deref(),
                )
                .await
                .map(|()| {
                    eprintln!("stored lesson: {atom}");
                });
                (r, 2)
            }
            StoreOp::Quiz(c) => {
                let r = store::cmd_store_quiz(
                    conn,
                    &c.atom,
                    c.difficulty,
                    c.question,
                    c.answer,
                    c.rubric,
                    c.quiz_type,
                    c.path.as_deref(),
                    c.graph.as_deref(),
                )
                .await
                .map(|qid| {
                    eprintln!("stored quiz: {qid}");
                });
                (r, 2)
            }
        },
        Cmd::Answer(c) => {
            let quiz = c.quiz.clone();
            let rating = c.rating;
            let r = answer::cmd_answer(
                conn,
                &c.quiz,
                c.rating,
                c.user_answer,
                c.path.as_deref(),
                c.graph.as_deref(),
            )
            .await
            .map(|()| {
                eprintln!("recorded {rating} for {quiz}");
            });
            (r, 2)
        }
        Cmd::Overlay(o) => match o.op {
            OverlayOp::Dump(_) => (overlay::cmd_dump(conn).await, 1),
        },
        Cmd::Amend(a) => match a.op {
            AmendOp::Quiz(c) => {
                let quiz = c.quiz.clone();
                let r = store::cmd_amend_quiz(
                    conn,
                    &c.quiz,
                    c.question,
                    c.answer,
                    c.rubric,
                    c.difficulty,
                    c.quiz_type,
                    c.path.as_deref(),
                    c.graph.as_deref(),
                )
                .await
                .map(|()| {
                    eprintln!("amended quiz: {quiz}");
                });
                (r, 2)
            }
        },
        Cmd::Remove(r) => match r.op {
            RemoveOp::Quiz(c) => {
                let quiz = c.quiz.clone();
                let r =
                    store::cmd_remove_quiz(conn, &c.quiz, c.path.as_deref(), c.graph.as_deref())
                        .await
                        .map(|()| {
                            eprintln!("removed quiz: {quiz}");
                        });
                (r, 2)
            }
        },
        Cmd::MigrateFromAyml(c) => (migrate::cmd_migrate(conn, c.from.as_deref()).await, 2),
    }
}
