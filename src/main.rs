use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use argh::FromArgs;

use mathtutor::cli;
#[cfg(feature = "mcp")]
use mathtutor::cli::McpOp;
use mathtutor::cli::{Cmd, GraphOp, LessonOp, Mt, PathOp, QuizOp, SubpathOp};
use mathtutor::graph::{Curriculum, CurriculumSource};
#[cfg(feature = "mcp")]
use mathtutor::mcp;
use mathtutor::{
    Error, Result, answer, db, discover, graph, import, instruct, overlay, path, scheduler, state,
    store, subpath, syllabus, tree,
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
    let cli = match parse_args() {
        Ok(cli) => cli,
        Err(code) => return code,
    };

    init_tracing(is_mcp(&cli.cmd));
    let curriculum = Curriculum::open(
        cli.graph
            .or_else(|| nonempty(std::env::var("MT_GRAPH").ok()).map(PathBuf::from)),
    );

    if let Cmd::Graph(g) = &cli.cmd
        && let GraphOp::Check(c) = &g.op
    {
        let source = c
            .path
            .clone()
            .map_or_else(|| curriculum.source().clone(), CurriculumSource::Dir);
        return match graph::run_check(&source) {
            Ok(report) => {
                report.print();
                if report.issues.is_empty() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(1)
                }
            }
            Err(e) => report_failure(&e),
        };
    }

    if matches!(cli.cmd, Cmd::Instruct(_)) {
        return match instruct::cmd_instruct() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => report_failure(&e),
        };
    }

    #[cfg(feature = "mcp")]
    if let Cmd::Mcp(c) = cli.cmd {
        if let Some(McpOp::Tools(_)) = c.op {
            return match mcp::print_tools() {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => report_failure(&e),
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
        let cfg = match db_config(curriculum) {
            Ok(c) => c,
            Err(e) => return report_failure(&e),
        };
        return match mcp::run(&c.addr, auth, cfg).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => report_failure(&e),
        };
    }

    if let Cmd::Graph(g) = &cli.cmd
        && let Some(result) = run_curriculum_lookup(&curriculum, &g.op).await
    {
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => report_failure(&e),
        };
    }

    let db = match db_config(curriculum) {
        Ok(cfg) if creates_database(&cli.cmd) => create_db(&cfg).await,
        Ok(cfg) => db::Db::connect(&cfg),
        Err(e) => Err(e),
    };
    let db = match db {
        Ok(db) => db,
        Err(e) => return report_failure(&e),
    };
    let recovery = unknown_outcome_recovery(&cli.cmd);
    match dispatch(&db, cli.cmd).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if e.outcome_is_unknown() {
                eprintln!("the command may have been applied; {recovery}");
            }
            report_failure(&e)
        }
    }
}

/// Read the database configuration from the environment, with `curriculum` as the curriculum.
fn db_config(curriculum: Arc<Curriculum>) -> Result<db::DbConfig> {
    Ok(db::DbConfig {
        curriculum,
        ..db::DbConfig::from_env()?
    })
}

/// Return `true` if `cmd` starts a learner database and can create it.
fn creates_database(cmd: &Cmd) -> bool {
    match cmd {
        Cmd::ImportTurso(_) => true,
        Cmd::Path(p) => matches!(p.op, PathOp::New(_)),
        _ => false,
    }
}

/// Connect to the database, create it if it does not exist, and report a creation on stderr.
async fn create_db(cfg: &db::DbConfig) -> Result<db::Db> {
    let db = db::Db::connect(cfg)?;
    if db.create_if_missing().await? {
        eprintln!("created database '{}' at {}", db.name(), db.url());
    }
    Ok(db)
}

/// Run a graph lookup without `--path` so it does not open a database connection.
async fn run_curriculum_lookup(curriculum: &Curriculum, op: &GraphOp) -> Option<Result<()>> {
    match op {
        GraphOp::Show(c) if c.path.is_none() => {
            Some(discover::cmd_graph_show(curriculum, &c.id, None).await)
        }
        GraphOp::List(c) if c.path.is_none() => {
            Some(discover::cmd_graph_list(curriculum, c.id.as_deref(), None, c.recursive).await)
        }
        _ => None,
    }
}

/// Print `e` to stderr and return its exit code.
fn report_failure(e: &Error) -> ExitCode {
    eprintln!("error: {e}");
    ExitCode::from(e.exit_code())
}

/// Return the safe next step after `cmd` fails with an unknown outcome.
fn unknown_outcome_recovery(cmd: &Cmd) -> &'static str {
    match cmd {
        Cmd::ImportTurso(_) => {
            "run `mt import-turso` again; a report that the import already completed means that it succeeded"
        }
        Cmd::Path(p) if matches!(p.op, PathOp::New(_)) => {
            "run `mt path list` and create the path only if it is absent"
        }
        Cmd::Quiz(q) if matches!(q.op, QuizOp::Answer(_) | QuizOp::Create(_)) => {
            "run `mt path next` instead of repeating the command"
        }
        _ => "repeat the command",
    }
}

/// Parse the command line, and return the exit code if the process should stop.
fn parse_args() -> std::result::Result<Mt, ExitCode> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    Mt::from_args(&["mt"], &args).map_err(|early| {
        if early.status.is_ok() {
            print!("{}", early.output);
            ExitCode::SUCCESS
        } else {
            eprint!("{}", early.output);
            ExitCode::from(2)
        }
    })
}

/// Filter out empty strings so unset environment variables do not match empty bearer tokens.
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

#[allow(clippy::too_many_lines)]
async fn dispatch(db: &db::Db, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Instruct(_) => unreachable!("handled before dispatch"),
        Cmd::ImportTurso(c) => {
            let url = c
                .url
                .or_else(|| nonempty(std::env::var("TURSO_URL").ok()))
                .ok_or(Error::MissingTursoUrl)?;
            import::cmd_import_turso(db, &url, nonempty(std::env::var("TURSO_AUTH_TOKEN").ok()))
                .await
        }
        #[cfg(feature = "mcp")]
        Cmd::Mcp(_) => unreachable!("handled before dispatch"),
        Cmd::Path(p) => dispatch_path(db, p.op).await,
        Cmd::Graph(g) => dispatch_graph(db, g.op).await,
        Cmd::Lesson(l) => dispatch_lesson(db, l.op).await,
        Cmd::Quiz(q) => dispatch_quiz(db, q.op).await,
    }
}

async fn dispatch_path(db: &db::Db, op: PathOp) -> Result<()> {
    match op {
        PathOp::List(_) => path::cmd_path_list(db).await,
        PathOp::New(c) => {
            let targets = cli::flatten_ids(&c.targets);
            path::cmd_path_new(db, &c.goal, &targets, c.strategy)
                .await
                .map(|id| {
                    eprintln!("created path: {id}");
                })
        }
        PathOp::State(c) => state::cmd_path_state(db, c.path.as_deref()).await,
        PathOp::Next(c) => {
            let mode = scheduler::NextMode::from_flags(c.new, c.due)?;
            scheduler::cmd_path_next(db, c.path.as_deref(), mode).await
        }
        PathOp::Strategy(c) => path::cmd_path_strategy(db, c.path.as_deref(), c.strategy)
            .await
            .map(|id| eprintln!("path {id}: strategy → {}", c.strategy)),
        PathOp::Subpath(c) => dispatch_subpath(db, c.op).await,
        PathOp::Syllabus(c) => syllabus::cmd_path_syllabus(db, c.path.as_deref(), c.n).await,
        PathOp::Tree(c) => tree::cmd_path_tree(db, c.path.as_deref(), c.depth).await,
    }
}

async fn dispatch_subpath(db: &db::Db, op: SubpathOp) -> Result<()> {
    match op {
        SubpathOp::Set(c) => {
            let atoms = cli::flatten_ids(&c.atoms);
            subpath::cmd_subpath_set(db, c.path.as_deref(), &atoms)
                .await
                .map(|id| eprintln!("path {id}: subpath set ({} atoms)", atoms.len()))
        }
        SubpathOp::Clear(c) => subpath::cmd_subpath_clear(db, c.path.as_deref())
            .await
            .map(|id| eprintln!("path {id}: subpath cleared")),
    }
}

async fn dispatch_graph(db: &db::Db, op: GraphOp) -> Result<()> {
    match op {
        GraphOp::Check(_) => unreachable!("handled before dispatch"),
        GraphOp::Show(c) => {
            let path = c.path.as_deref().map(|p| (db, p));
            discover::cmd_graph_show(db.curriculum(), &c.id, path).await
        }
        GraphOp::List(c) => {
            let path = c.path.as_deref().map(|p| (db, p));
            discover::cmd_graph_list(db.curriculum(), c.id.as_deref(), path, c.recursive).await
        }
        GraphOp::Dump(_) => overlay::cmd_graph_dump(db).await,
    }
}

async fn dispatch_lesson(db: &db::Db, op: LessonOp) -> Result<()> {
    match op {
        LessonOp::Upsert(c) => {
            let atom = c.atom.clone();
            store::cmd_lesson_upsert(db, &c.atom, c.body, c.path.as_deref())
                .await
                .map(|()| {
                    eprintln!("upserted lesson: {atom}");
                })
        }
    }
}

async fn dispatch_quiz(db: &db::Db, op: QuizOp) -> Result<()> {
    match op {
        QuizOp::Create(c) => store::cmd_quiz_create(
            db,
            &c.atom,
            c.difficulty,
            c.question,
            c.answer,
            c.rubric,
            c.quiz_type,
            c.path.as_deref(),
        )
        .await
        .map(|qid| {
            eprintln!("created quiz: {qid}");
        }),
        QuizOp::Update(c) => {
            let quiz = c.quiz.clone();
            store::cmd_quiz_update(
                db,
                &c.quiz,
                c.question,
                c.answer,
                c.rubric,
                c.difficulty,
                c.quiz_type,
                c.path.as_deref(),
            )
            .await
            .map(|()| {
                eprintln!("updated quiz: {quiz}");
            })
        }
        QuizOp::Delete(c) => {
            let quiz = c.quiz.clone();
            store::cmd_quiz_delete(db, &c.quiz, c.path.as_deref())
                .await
                .map(|()| {
                    eprintln!("deleted quiz: {quiz}");
                })
        }
        QuizOp::Answer(c) => {
            let quiz = c.quiz.clone();
            let rating = c.rating;
            answer::cmd_quiz_answer(db, &c.quiz, c.rating, c.user_answer, c.path.as_deref())
                .await
                .map(|()| {
                    eprintln!("recorded {rating} for {quiz}");
                })
        }
    }
}
