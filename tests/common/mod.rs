//! Shared test helpers and in-memory Baserunna server fixture.

#![allow(dead_code)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use baserunna::env::Env;
use baserunna::limits::LimitConfig;
use baserunna::node::{Node, NodeConfig, Services};
use baserunna::store::MemStore;
use chrono::{DateTime, Utc};
use mathtutor::cards::Projection;
use mathtutor::db::{Db, DbConfig};
use mathtutor::event_log::{Event, EventKind};
use mathtutor::graph::{FlatConcept, Graph, Quiz};
use mathtutor::overlay::Overlay;
use mathtutor::path::PathFile;
use mathtutor::progress::PathProgress;
use mathtutor::scheduler::{Action, NextMode, PathPlan};
use mathtutor::types::{Difficulty, Rating, Strategy};
use mathtutor::view::{Changes, PathState, View, Want};
use tempfile::TempDir;

pub const PATH_ID: &str = "p_test";

// Compiles only if the test server and mathtutor share the same API crate, so tests run against the client that ships.
const _: fn(&baserunna::error::Error) -> mathtutor::db::client::api::ApiError =
    baserunna::api::api_error;

struct Server {
    url: String,
    token: String,
}

fn server() -> &'static Server {
    static SERVER: OnceLock<Server> = OnceLock::new();
    SERVER.get_or_init(start_server)
}

// Run the shared node on a dedicated runtime so it outlives individual test runtimes.
fn start_server() -> Server {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            let url = format!("http://{}", listener.local_addr().expect("addr"));
            baserunna::log::set_level(Some(baserunna::log::Level::Warn));
            let cfg = NodeConfig {
                name: "test".into(),
                addr: url.clone(),
                public_url: url.clone(),
                pbkdf2_iterations: 1000,
                limits: LimitConfig::unlimited(),
                ..NodeConfig::default()
            };
            let store = Arc::new(MemStore::with_system_clock());
            let node = Node::start(cfg, Arc::new(Env::real()), store, Services::local())
                .await
                .expect("start baserunna");
            let app = baserunna::api::router(node)
                .into_make_service_with_connect_info::<std::net::SocketAddr>();
            tokio::spawn(async move { axum::serve(listener, app).await });
            let token = sign_up(&url).await;
            tx.send(Server { url, token }).expect("send server");
            std::future::pending::<()>().await;
        });
    });
    rx.recv().expect("test baserunna node did not start")
}

async fn sign_up(url: &str) -> String {
    let client = baserunna_client::Client::new(baserunna_client::HttpTransport::new(url));
    client
        .sign_up("admin@test.dev", "password-123")
        .await
        .expect("sign up")
        .token
}

/// Write a curriculum directory with a single area whose `children` list is `area_body`.
pub fn write_graph(area_body: &str) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let areas = dir.path().join("areas");
    std::fs::create_dir(&areas).expect("areas/");
    let manifest = "
schema_version: 1
areas:
  - prefix: ta
    slug: test-area
    file: areas/test.ayml
    summary: \"t\"
";
    std::fs::write(dir.path().join("manifest.ayml"), manifest).expect("manifest");
    let area = format!(
        "
schema_version: 2
area: test-area
prefix: ta
summary: \"t\"
motivation: \"t\"
children:
{area_body}"
    );
    std::fs::write(areas.join("test.ayml"), area).expect("area");
    dir
}

/// Return a configuration for an isolated test database.
pub fn fresh_config() -> DbConfig {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let s = server();
    DbConfig {
        url: s.url.clone(),
        token: s.token.clone(),
        db: format!(
            "t{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ),
        cache_dir: None,
        curriculum: mathtutor::graph::Curriculum::embedded(),
    }
}

/// Create the database of `cfg` with every table and return its handle.
pub async fn create_db(cfg: &DbConfig) -> Db {
    let db = Db::connect(cfg).expect("connect");
    db.create_if_missing().await.expect("create database");
    db
}

/// Create an empty test database.
pub async fn empty_db() -> Db {
    create_db(&fresh_config()).await
}

/// Open a test database initialized with a bottom-up path `path_id`.
pub async fn fresh_db(path_id: &str) -> Db {
    let db = empty_db().await;
    seed_path(&db, path_id, &[], Strategy::BottomUp).await;
    db
}

/// Insert a path with `targets` and `strategy`.
pub async fn seed_path(db: &Db, path_id: &str, targets: &[&str], strategy: Strategy) {
    let created = DateTime::parse_from_rfc3339("2026-05-26T00:00:00Z")
        .unwrap()
        .to_utc();
    let p = PathFile {
        id: path_id.into(),
        goal: "test goal".into(),
        created_at: created,
        targets: targets.iter().map(|s| (*s).to_string()).collect(),
        strategy,
        subpath: Vec::new(),
    };
    db.transact(&Want::nothing(), |view, changes| {
        changes.create_path(view, &p)
    })
    .await
    .expect("seed path");
}

/// Commit mutations from `f` against `path_id`.
pub async fn write(db: &Db, path_id: &str, mut f: impl FnMut(&View, &mut Changes)) {
    db.transact(&Want::path(Some(path_id)), |view, changes| {
        f(view, changes);
        Ok(())
    })
    .await
    .expect("write");
}

/// Append `events` to their path stream.
pub async fn append(db: &Db, events: &[Event]) {
    let path_id = events
        .first()
        .map_or(PATH_ID, |e| e.path.as_str())
        .to_string();
    db.transact(&Want::path(Some(&path_id)), |view, changes| {
        events.iter().try_for_each(|e| changes.append(view, e))
    })
    .await
    .expect("append");
}

/// Load the overlay from its stored rows, and assert that it equals the overlay that `db` caches.
pub async fn overlay(db: &Db) -> Overlay {
    let read = async |db: &Db| {
        let view = db.snapshot(&Want::overlay()).await.expect("snapshot");
        view.overlay().expect("overlay").clone()
    };
    let cached = read(db).await;
    let stored = read(&db.without_overlay_cache()).await;
    assert_eq!(
        serde_json::to_value(&cached).unwrap(),
        serde_json::to_value(&stored).unwrap(),
        "the cached overlay differs from the stored rows"
    );
    stored
}

/// Load the stored state for `path_id`.
pub async fn path_state(db: &Db, path_id: &str) -> PathState {
    db.snapshot(&Want::path(Some(path_id)))
        .await
        .expect("snapshot")
        .path()
        .expect("path")
        .clone()
}

pub fn quiz(id: &str, difficulty: Difficulty) -> Quiz {
    Quiz {
        id: id.into(),
        difficulty,
        kind: None,
        question: "q".into(),
        answer: "a".into(),
        rubric: None,
    }
}

pub fn atom(id: &str, prereqs: &[&str], lesson: Option<&str>, quizzes: Vec<Quiz>) -> FlatConcept {
    FlatConcept {
        id: id.into(),
        name: id.into(),
        description: None,
        prerequisites: prereqs.iter().map(|s| (*s).to_string()).collect(),
        children_ids: Vec::new(),
        lesson: lesson.map(String::from),
        quizzes,
    }
}

pub fn cluster(id: &str, children: &[&str]) -> FlatConcept {
    FlatConcept {
        id: id.into(),
        name: id.into(),
        description: None,
        prerequisites: Vec::new(),
        children_ids: children.iter().map(|s| (*s).to_string()).collect(),
        lesson: None,
        quizzes: Vec::new(),
    }
}

/// Build an atom without a lesson or quizzes.
pub fn empty_atom(id: &str, prereqs: &[&str]) -> FlatConcept {
    atom(id, prereqs, None, Vec::new())
}

/// Build an atom with a lesson body and an easy, medium, and hard quiz.
pub fn complete_atom(id: &str, prereqs: &[&str]) -> FlatConcept {
    atom(
        id,
        prereqs,
        Some("body"),
        vec![
            quiz(&format!("{id}.q1"), Difficulty::Easy),
            quiz(&format!("{id}.q2"), Difficulty::Medium),
            quiz(&format!("{id}.q3"), Difficulty::Hard),
        ],
    )
}

pub fn path_with(targets: &[&str]) -> PathFile {
    path_with_strategy(targets, Strategy::BottomUp)
}

pub fn path_with_strategy(targets: &[&str], strategy: Strategy) -> PathFile {
    PathFile {
        id: PATH_ID.into(),
        goal: "test".into(),
        created_at: Utc::now(),
        targets: targets.iter().map(|s| (*s).to_string()).collect(),
        strategy,
        subpath: Vec::new(),
    }
}

/// Return `e` with its timestamp set to `ts`.
pub fn at(ts: DateTime<Utc>, e: Event) -> Event {
    Event { ts, ..e }
}

pub fn taught(atom_id: &str) -> Event {
    Event::now(
        PATH_ID,
        EventKind::LessonTaught {
            atom: atom_id.into(),
        },
    )
}

pub fn lesson_authored(atom_id: &str) -> Event {
    Event::now(
        PATH_ID,
        EventKind::LessonAuthored {
            atom: atom_id.into(),
        },
    )
}

pub fn answered(quiz_id: &str, rating: Rating) -> Event {
    Event::now(
        PATH_ID,
        EventKind::QuizAnswered {
            atom: None,
            quiz: quiz_id.into(),
            rating,
            user_answer: None,
        },
    )
}

/// Return the presentation of quiz `quiz_id`, whose ID is its atom ID with one more segment.
pub fn presented(quiz_id: &str) -> Event {
    Event::now(
        PATH_ID,
        EventKind::QuizPresented {
            atom: quiz_id
                .rsplit_once('.')
                .map_or(quiz_id, |(atom, _)| atom)
                .into(),
            quiz: quiz_id.into(),
        },
    )
}

/// Build the events that complete `atom_id`.
pub fn complete_events(atom_id: &str) -> Vec<Event> {
    vec![
        taught(atom_id),
        answered(&format!("{atom_id}.q1"), Rating::Good),
        answered(&format!("{atom_id}.q2"), Rating::Good),
        answered(&format!("{atom_id}.q3"), Rating::Good),
    ]
}

/// Return the progress that the production projection derives from `events`.
pub fn progress_of(events: &[Event]) -> PathProgress {
    Projection::from_events(events)
        .expect("project events")
        .progress()
}

/// Return the state of path `p` after `events`.
pub fn state_of(p: &PathFile, events: &[Event]) -> PathState {
    PathState {
        file: p.clone(),
        updated_at: p.created_at,
        projection: Projection::from_events(events).expect("project events"),
    }
}

/// Return the next action in `mode` of path `p` over `g` after `events`, at time `now`.
pub fn next_action_at(
    g: &Graph,
    p: &PathFile,
    events: &[Event],
    now: DateTime<Utc>,
    mode: NextMode,
) -> Action {
    let state = state_of(p, events);
    PathPlan::new(&state, Arc::new(g.clone()), now)
        .expect("plan")
        .next_action(mode)
}

/// Return the next action in `mode` of path `p` over `g` after `events`.
pub fn next_action(g: &Graph, p: &PathFile, events: &[Event], mode: NextMode) -> Action {
    next_action_at(g, p, events, Utc::now(), mode)
}

/// Return the atoms of path `p` over `g` with untaught lessons after `events`, in teaching order.
pub fn upcoming(g: &Graph, p: &PathFile, events: &[Event]) -> Vec<String> {
    let state = state_of(p, events);
    PathPlan::new(&state, Arc::new(g.clone()), Utc::now())
        .expect("plan")
        .upcoming()
}
