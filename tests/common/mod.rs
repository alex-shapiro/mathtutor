//! Shared helpers for integration tests.
//!
//! Each test binary in `tests/` compiles this module independently and
//! flags different dead code. To avoid, we allow dead code here.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use baserunna::env::Env;
use baserunna::limits::LimitConfig;
use baserunna::node::{Node, NodeConfig, Services};
use baserunna::store::MemStore;
use chrono::{DateTime, Utc};
use mathtutor::db::{Db, DbConfig};
use mathtutor::event_log::{Event, EventKind, EventPayload};
use mathtutor::graph::{FlatConcept, Graph, Quiz};
use mathtutor::overlay::Overlay;
use mathtutor::path::{self, PathFile};
use mathtutor::progress::PathProgress;
use mathtutor::types::{Difficulty, Rating, Strategy};
use mathtutor::view::{Changes, PathSel, PathState, View, Want};

pub const PATH_ID: &str = "p_test";

// ── Test server ─────────────────────────────────────────────────────

/// A Baserunna node with an in-memory store, shared by every test in the
/// binary, and a session token on it.
struct Server {
    url: String,
    token: String,
}

fn server() -> &'static Server {
    static SERVER: OnceLock<Server> = OnceLock::new();
    SERVER.get_or_init(start_server)
}

/// Run the node on its own thread and runtime, which outlive any one
/// test's runtime and end with the test binary.
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

/// Sign up the node's first user, its admin, and return the session token.
async fn sign_up(url: &str) -> String {
    #[derive(serde::Serialize)]
    struct Credentials<'a> {
        email: &'a str,
        password: &'a str,
    }
    #[derive(serde::Deserialize)]
    struct Session {
        token: String,
    }
    let creds = Credentials {
        email: "admin@test.dev",
        password: "password-123",
    };
    reqwest::Client::new()
        .post(format!("{url}/v1/auth/signup"))
        .json(&creds)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .expect("sign up")
        .json::<Session>()
        .await
        .expect("session")
        .token
}

/// Config for a database no other test uses.
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
    }
}

/// A fresh, empty user database.
pub async fn empty_db() -> Db {
    Db::open(&fresh_config()).await.expect("open")
}

/// A fresh user database seeded with one bottom-up path, `path_id`, with no targets.
pub async fn fresh_db(path_id: &str) -> Db {
    let db = empty_db().await;
    seed_path(&db, path_id, &[], Strategy::BottomUp).await;
    db
}

/// Store a path created at 2026-05-26.
pub async fn seed_path(db: &Db, path_id: &str, targets: &[&str], strategy: Strategy) {
    let created = DateTime::parse_from_rfc3339("2026-05-26T00:00:00Z")
        .unwrap()
        .to_utc();
    let p = PathFile {
        id: path_id.into(),
        goal: "test goal".into(),
        created_at: created,
        updated_at: created,
        targets: targets.iter().map(|s| (*s).to_string()).collect(),
        strategy,
        subpath: Vec::new(),
    };
    let want = Want {
        path: PathSel::None,
        overlay: false,
        all_paths: false,
    };
    db.transact(&want, |view, changes| path::insert(view, changes, &p))
        .await
        .expect("seed path");
}

/// Commit `f`'s changes against `path_id`'s state.
pub async fn write(db: &Db, path_id: &str, mut f: impl FnMut(&View, &mut Changes)) {
    db.transact(&Want::path(Some(path_id)), |view, changes| {
        f(view, changes);
        Ok(())
    })
    .await
    .expect("write");
}

/// Log `events` (each naming its path) in one transaction.
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

/// The stored overlay.
pub async fn overlay(db: &Db) -> Overlay {
    std::sync::Arc::unwrap_or_clone(
        db.snapshot(&Want::overlay())
            .await
            .expect("snapshot")
            .overlay,
    )
}

/// `path_id`'s stored state.
pub async fn path_state(db: &Db, path_id: &str) -> PathState {
    db.snapshot(&Want::path(Some(path_id)))
        .await
        .expect("snapshot")
        .path()
        .expect("path")
        .clone()
}

// ── Concept builders ────────────────────────────────────────────────

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

/// `atom` with no lesson and no quizzes.
/// Represents a leaf that must be authored from scratch.
pub fn empty_atom(id: &str, prereqs: &[&str]) -> FlatConcept {
    atom(id, prereqs, None, Vec::new())
}

/// `atom` pre-populated with a lesson body and one quiz per difficulty,
/// IDs derived as `{id}.q1`/`.q2`/`.q3`.
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

pub fn graph_of(concepts: Vec<FlatConcept>) -> Graph {
    let mut by_id = HashMap::new();
    for c in concepts {
        by_id.insert(c.id.clone(), c);
    }
    Graph { by_id }
}

pub fn path_with(targets: &[&str]) -> PathFile {
    path_with_strategy(targets, Strategy::BottomUp)
}

pub fn path_with_strategy(targets: &[&str], strategy: Strategy) -> PathFile {
    PathFile {
        id: PATH_ID.into(),
        goal: "test".into(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        targets: targets.iter().map(|s| (*s).to_string()).collect(),
        strategy,
        subpath: Vec::new(),
    }
}

// ── Event builders ──────────────────────────────────────────────────

pub fn taught(atom_id: &str) -> Event {
    taught_at(atom_id, Utc::now())
}

pub fn taught_at(atom_id: &str, ts: DateTime<Utc>) -> Event {
    Event {
        ts,
        kind: EventKind::LessonTaught,
        path: PATH_ID.into(),
        atom: Some(atom_id.into()),
        quiz: None,
        payload: EventPayload::default(),
    }
}

pub fn lesson_authored(atom_id: &str) -> Event {
    Event {
        ts: Utc::now(),
        kind: EventKind::LessonAuthored,
        path: PATH_ID.into(),
        atom: Some(atom_id.into()),
        quiz: None,
        payload: EventPayload::default(),
    }
}

pub fn answered(quiz_id: &str, rating: Rating) -> Event {
    answered_at(quiz_id, rating, Utc::now())
}

pub fn answered_at(quiz_id: &str, rating: Rating, ts: DateTime<Utc>) -> Event {
    Event {
        ts,
        kind: EventKind::QuizAnswered,
        path: PATH_ID.into(),
        atom: None,
        quiz: Some(quiz_id.into()),
        payload: EventPayload {
            rating: Some(rating),
            ..Default::default()
        },
    }
}

pub fn presented(quiz_id: &str) -> Event {
    presented_at(quiz_id, Utc::now())
}

pub fn presented_at(quiz_id: &str, ts: DateTime<Utc>) -> Event {
    Event {
        ts,
        kind: EventKind::QuizPresented,
        path: PATH_ID.into(),
        atom: None,
        quiz: Some(quiz_id.into()),
        payload: EventPayload::default(),
    }
}

/// The set of events that mark `atom_id` complete:
/// lesson taught + an answer on each of its quizzes.
pub fn complete_events(atom_id: &str) -> Vec<Event> {
    vec![
        taught(atom_id),
        answered(&format!("{atom_id}.q1"), Rating::Good),
        answered(&format!("{atom_id}.q2"), Rating::Good),
        answered(&format!("{atom_id}.q3"), Rating::Good),
    ]
}

// ── Progress ────────────────────────────────────────────────────────

/// Fold a synthetic event log into a `PathProgress`, as the stored
/// `taught` and `cards` rows would, without a database.
pub fn progress_of(events: &[Event]) -> PathProgress {
    let mut p = PathProgress::default();
    for e in events {
        match e.kind {
            EventKind::LessonTaught | EventKind::LessonAuthored => {
                if let Some(atom) = &e.atom {
                    p.taught_atoms.insert(atom.clone());
                }
            }
            EventKind::QuizAnswered => {
                if let (Some(q), Some(r)) = (&e.quiz, e.payload.rating)
                    && r.is_correct()
                {
                    p.correct_quizzes.insert(q.clone());
                }
            }
            _ => {}
        }
    }
    p
}
