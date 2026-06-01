use std::process::ExitCode;

use libsql::Connection;
#[cfg(feature = "mcp")]
use mathtutor::cli::McpOp;
use mathtutor::cli::{Cmd, GraphOp, LessonOp, Mt, PathOp, QuizOp};
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
    if let Cmd::Graph(g) = &cli.cmd
        && let GraphOp::Check(c) = &g.op
    {
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

    // `mt mcp` owns its own DB lifecycle (long-running, background sync
    // task, graceful shutdown) so it sits outside the per-command DB
    // setup the rest of the dispatch block does.
    #[cfg(feature = "mcp")]
    if let Cmd::Mcp(c) = cli.cmd {
        if let Some(McpOp::Tools(_)) = c.op {
            return match mcp::print_tools() {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::from(2)
                }
            };
        }
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
    // Pull the latest remote state before running.
    db::maybe_sync(&db, &cfg).await;
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

/// Commands that write to the database. `mt path next` mutates because
/// it auto-logs `quiz_presented` / `lesson_taught`; read-only path
/// queries (`list`, `state`, `tree`) and the operator-only graph
/// inspectors (`show`, `list`, `dump`) don't trigger a foreground sync.
fn mutating(cmd: &Cmd) -> bool {
    match cmd {
        Cmd::Path(p) => matches!(p.op, PathOp::New(_) | PathOp::Next(_)),
        Cmd::Lesson(_) | Cmd::Quiz(_) | Cmd::MigrateFromAyml(_) => true,
        Cmd::Graph(_) | Cmd::Instruct(_) => false,
        #[cfg(feature = "mcp")]
        Cmd::Mcp(_) => false,
    }
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

/// Run the chosen subcommand. Returns the result plus the exit code to
/// use on failure — most commands exit `2` (config / IO / validation),
/// `mt path next` / `mt path state` / `mt path tree` / `mt graph dump`
/// exit `1` to distinguish "scheduling / state read failure" from
/// "you held it wrong."
#[allow(clippy::too_many_lines)]
async fn dispatch(conn: &Connection, cmd: Cmd) -> (Result<()>, u8) {
    match cmd {
        Cmd::Instruct(_) => unreachable!("handled before dispatch"),
        #[cfg(feature = "mcp")]
        Cmd::Mcp(_) => unreachable!("handled before dispatch"),
        Cmd::Path(p) => dispatch_path(conn, p.op).await,
        Cmd::Graph(g) => dispatch_graph(conn, g.op).await,
        Cmd::Lesson(l) => dispatch_lesson(conn, l.op).await,
        Cmd::Quiz(q) => dispatch_quiz(conn, q.op).await,
        Cmd::MigrateFromAyml(c) => (migrate::cmd_migrate(conn, c.from.as_deref()).await, 2),
    }
}

async fn dispatch_path(conn: &Connection, op: PathOp) -> (Result<()>, u8) {
    match op {
        PathOp::List(c) => (path::cmd_path_list(conn, c.graph.as_deref()).await, 1),
        PathOp::New(c) => {
            let r = path::cmd_path_new(conn, &c.goal, &c.atom, c.graph.as_deref())
                .await
                .map(|id| {
                    eprintln!("created path: {id}");
                });
            (r, 2)
        }
        PathOp::State(c) => (
            state::cmd_path_state(conn, c.path.as_deref(), c.graph.as_deref()).await,
            1,
        ),
        PathOp::Next(c) => (
            scheduler::cmd_path_next(conn, c.path.as_deref(), c.graph.as_deref()).await,
            1,
        ),
        PathOp::Tree(c) => (
            tree::cmd_path_tree(conn, c.path.as_deref(), c.graph.as_deref()).await,
            1,
        ),
    }
}

async fn dispatch_graph(conn: &Connection, op: GraphOp) -> (Result<()>, u8) {
    match op {
        GraphOp::Check(_) => unreachable!("handled before dispatch"),
        GraphOp::Show(c) => (
            discover::cmd_graph_show(conn, &c.id, c.path.as_deref(), c.graph.as_deref()).await,
            2,
        ),
        GraphOp::List(c) => (
            discover::cmd_graph_list(conn, c.id.as_deref(), c.path.as_deref(), c.graph.as_deref())
                .await,
            2,
        ),
        GraphOp::Dump(_) => (overlay::cmd_graph_dump(conn).await, 1),
    }
}

async fn dispatch_lesson(conn: &Connection, op: LessonOp) -> (Result<()>, u8) {
    match op {
        LessonOp::Upsert(c) => {
            let atom = c.atom.clone();
            let r = store::cmd_lesson_upsert(
                conn,
                &c.atom,
                c.body,
                c.path.as_deref(),
                c.graph.as_deref(),
            )
            .await
            .map(|()| {
                eprintln!("upserted lesson: {atom}");
            });
            (r, 2)
        }
    }
}

async fn dispatch_quiz(conn: &Connection, op: QuizOp) -> (Result<()>, u8) {
    match op {
        QuizOp::Create(c) => {
            let r = store::cmd_quiz_create(
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
                eprintln!("created quiz: {qid}");
            });
            (r, 2)
        }
        QuizOp::Update(c) => {
            let quiz = c.quiz.clone();
            let r = store::cmd_quiz_update(
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
                eprintln!("updated quiz: {quiz}");
            });
            (r, 2)
        }
        QuizOp::Delete(c) => {
            let quiz = c.quiz.clone();
            let r = store::cmd_quiz_delete(conn, &c.quiz, c.path.as_deref(), c.graph.as_deref())
                .await
                .map(|()| {
                    eprintln!("deleted quiz: {quiz}");
                });
            (r, 2)
        }
        QuizOp::Answer(c) => {
            let quiz = c.quiz.clone();
            let rating = c.rating;
            let r = answer::cmd_quiz_answer(
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
    }
}
