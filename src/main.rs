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
use mathtutor::surface::Surface;
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
    let ctx = Context {
        curriculum: Curriculum::open(
            cli.graph
                .or_else(|| nonempty(std::env::var("MT_GRAPH").ok()).map(PathBuf::from)),
        ),
    };
    let recovery = cli.cmd.operation().unknown_outcome_step(Surface::Cli);
    match dispatch(&ctx, cli.cmd).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if e.outcome_is_unknown() {
                eprintln!("the command may have been applied; {recovery}");
            }
            report_failure(&e)
        }
    }
}

/// Settings that every command of the process shares.
struct Context {
    curriculum: Arc<Curriculum>,
}

impl Context {
    /// Read the database configuration from the environment, with the curriculum of this process.
    fn db_config(&self) -> Result<db::DbConfig> {
        Ok(db::DbConfig {
            curriculum: self.curriculum.clone(),
            ..db::DbConfig::from_env()?
        })
    }

    /// Connect to the learner database without network I/O.
    fn db(&self) -> Result<db::Db> {
        db::Db::connect(&self.db_config()?)
    }

    /// Connect to the learner database, create it if it does not exist, and report a creation on stderr.
    async fn create_db(&self) -> Result<db::Db> {
        let db = self.db()?;
        if db.create_if_missing().await? {
            eprintln!("created database '{}' at {}", db.name(), db.url());
        }
        Ok(db)
    }
}

/// Print `e` to stderr and return its exit code.
fn report_failure(e: &Error) -> ExitCode {
    eprintln!("error: {e}");
    ExitCode::from(e.exit_code())
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

async fn dispatch(ctx: &Context, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Instruct(_) => instruct::cmd_instruct(),
        Cmd::ImportTurso(c) => {
            let url = c
                .url
                .or_else(|| nonempty(std::env::var("TURSO_URL").ok()))
                .ok_or(Error::MissingTursoUrl)?;
            let token = nonempty(std::env::var("TURSO_AUTH_TOKEN").ok());
            import::cmd_import_turso(&ctx.create_db().await?, &url, token).await
        }
        #[cfg(feature = "mcp")]
        Cmd::Mcp(c) => run_mcp(ctx, c).await,
        Cmd::Path(p) => dispatch_path(ctx, p.op).await,
        Cmd::Graph(g) => dispatch_graph(ctx, g.op).await,
        Cmd::Lesson(l) => dispatch_lesson(&ctx.db()?, l.op).await,
        Cmd::Quiz(q) => dispatch_quiz(&ctx.db()?, q.op).await,
    }
}

#[cfg(feature = "mcp")]
async fn run_mcp(ctx: &Context, c: cli::McpCmd) -> Result<()> {
    if let Some(McpOp::Tools(_)) = c.op {
        return mcp::print_tools();
    }
    let auth = mcp::AuthConfig {
        api_key: nonempty(c.api_key.or_else(|| std::env::var("MT_API_KEY").ok())),
        admin_password: nonempty(
            c.admin_password
                .or_else(|| std::env::var("MT_ADMIN_PASSWORD").ok()),
        ),
        public_url: nonempty(c.public_url.or_else(|| std::env::var("MT_PUBLIC_URL").ok())),
    };
    mcp::run(&c.addr, auth, ctx.db_config()?).await
}

async fn dispatch_path(ctx: &Context, op: PathOp) -> Result<()> {
    let db = &match op {
        PathOp::New(_) => ctx.create_db().await?,
        _ => ctx.db()?,
    };
    match op {
        PathOp::List(_) => path::cmd_path_list(db).await,
        PathOp::New(c) => {
            let targets = cli::flatten_ids(&c.targets);
            let id = path::cmd_path_new(db, &c.goal, &targets, c.strategy).await?;
            eprintln!("created path: {id}");
            Ok(())
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

async fn dispatch_graph(ctx: &Context, op: GraphOp) -> Result<()> {
    match op {
        GraphOp::Check(c) => {
            let source = c
                .path
                .map_or_else(|| ctx.curriculum.source().clone(), CurriculumSource::Dir);
            graph::cmd_graph_check(&source)
        }
        GraphOp::Show(c) => {
            let db = c.path.as_ref().map(|_| ctx.db()).transpose()?;
            let path = db.as_ref().zip(c.path.as_deref());
            discover::cmd_graph_show(&ctx.curriculum, &c.id, path).await
        }
        GraphOp::List(c) => {
            let db = c.path.as_ref().map(|_| ctx.db()).transpose()?;
            let path = db.as_ref().zip(c.path.as_deref());
            discover::cmd_graph_list(&ctx.curriculum, c.id.as_deref(), path, c.recursive).await
        }
        GraphOp::Dump(_) => overlay::cmd_graph_dump(&ctx.db()?).await,
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
