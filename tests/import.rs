//! `mt import-turso` against a fake libSQL server speaking Turso's HTTP
//! API, serving canned rows in the schema of the last libSQL migration.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use mathtutor::Error;
use mathtutor::cards::{self, CardRow};
use mathtutor::db::{Db, ts};
use mathtutor::event_log::{self, Event, EventKind, EventPayload};
use mathtutor::import::{self, Turso};
use mathtutor::types::{Rating, Strategy};
use mathtutor::view::{PathSel, Want};
use serde::{Deserialize, Serialize};

mod common;

const TOKEN: &str = "turso-token";

// ── Fake server ─────────────────────────────────────────────────────

#[derive(Deserialize)]
struct PipelineReq {
    requests: Vec<Req>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Req {
    Execute { stmt: Stmt },
    Close,
}

#[derive(Deserialize)]
struct Stmt {
    sql: String,
}

#[derive(Serialize)]
struct PipelineResp {
    results: Vec<Res>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Res {
    Ok { response: Resp },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Resp {
    Execute { result: StmtResult },
    Close,
}

#[derive(Serialize, Default, Clone)]
struct StmtResult {
    cols: Vec<Col>,
    rows: Vec<Vec<V>>,
}

#[derive(Serialize, Clone)]
struct Col {
    name: String,
}

#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
enum V {
    Null,
    Integer { value: String },
    Float { value: f64 },
    Text { value: String },
}

fn text(s: &str) -> V {
    V::Text { value: s.into() }
}

fn opt(s: Option<&str>) -> V {
    s.map_or(V::Null, text)
}

fn int(n: i64) -> V {
    V::Integer {
        value: n.to_string(),
    }
}

/// Rows of each libSQL table, keyed by the table a query reads `FROM`.
#[derive(Default, Clone)]
struct Tables(HashMap<&'static str, StmtResult>);

impl Tables {
    fn set(&mut self, table: &'static str, cols: &[&str], rows: Vec<Vec<V>>) {
        let cols = cols.iter().map(|c| Col { name: (*c).into() }).collect();
        self.0.insert(table, StmtResult { cols, rows });
    }
}

async fn pipeline(
    State(tables): State<Arc<Tables>>,
    headers: HeaderMap,
    Json(req): Json<PipelineReq>,
) -> Result<Json<PipelineResp>, StatusCode> {
    let auth = headers.get("authorization").and_then(|v| v.to_str().ok());
    if auth != Some(&format!("Bearer {TOKEN}")) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let results = req
        .requests
        .into_iter()
        .map(|r| Res::Ok {
            response: match r {
                Req::Execute { stmt } => {
                    let table = stmt
                        .sql
                        .split_once("FROM ")
                        .and_then(|(_, rest)| rest.split_whitespace().next());
                    let result = table
                        .and_then(|t| tables.0.get(t).cloned())
                        .unwrap_or_default();
                    Resp::Execute { result }
                }
                Req::Close => Resp::Close,
            },
        })
        .collect();
    Ok(Json(PipelineResp { results }))
}

async fn serve(tables: Tables) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v2/pipeline", post(pipeline))
        .with_state(Arc::new(tables));
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

// ── Legacy data ─────────────────────────────────────────────────────

fn at(day: i64) -> DateTime<Utc> {
    ts::parse("2026-01-01T08:30:00.250Z").unwrap() + Duration::days(day)
}

fn event(path: &str, day: i64, kind: EventKind, atom: Option<&str>, quiz: Option<&str>) -> Event {
    Event {
        ts: at(day),
        kind,
        path: path.into(),
        atom: atom.map(Into::into),
        quiz: quiz.map(Into::into),
        payload: EventPayload::default(),
    }
}

fn answer(path: &str, day: i64, quiz: &str, rating: Rating, user_answer: Option<&str>) -> Event {
    Event {
        payload: EventPayload {
            rating: Some(rating),
            user_answer: user_answer.map(Into::into),
            ..EventPayload::default()
        },
        ..event(path, day, EventKind::QuizAnswered, None, Some(quiz))
    }
}

/// `p_a`, top-down with a lesson and several answers, and `p_b`, created
/// and never studied. A third path, `p_c`, has no events at all.
fn events() -> Vec<Event> {
    vec![
        event("p_a", 0, EventKind::PathCreated, None, None),
        event("p_a", 0, EventKind::LessonTaught, Some("fnd.2.1.1"), None),
        event(
            "p_a",
            0,
            EventKind::QuizPresented,
            Some("fnd.2.1.1"),
            Some("fnd.2.1.1.q1"),
        ),
        answer("p_a", 0, "fnd.2.1.1.q1", Rating::Good, Some("42")),
        answer("p_a", 3, "fnd.2.1.1.q1", Rating::Again, None),
        answer("p_a", 4, "fnd.2.1.1.q2", Rating::Easy, None),
        answer("p_a", 9, "fnd.2.1.1.q1", Rating::Hard, None),
        event("p_b", 30, EventKind::PathCreated, None, None),
    ]
}

fn event_rows() -> Vec<Vec<V>> {
    events()
        .iter()
        .map(|e| {
            let kind = serde_json::to_value(e.kind).unwrap();
            let payload = e
                .payload
                .user_answer
                .as_ref()
                .map(|a| format!(r#"{{"user_answer":"{a}"}}"#));
            vec![
                text(&ts::format(e.ts)),
                text(kind.as_str().unwrap()),
                text(&e.path),
                opt(e.atom.as_deref()),
                opt(e.quiz.as_deref()),
                e.payload.rating.map_or(V::Null, |r| int(i64::from(r))),
                opt(payload.as_deref()),
            ]
        })
        .collect()
}

fn legacy() -> Tables {
    let mut t = Tables::default();
    t.set("schema_migrations", &["version"], vec![vec![int(4)]]);
    t.set(
        "paths",
        &["id", "goal", "created_at", "strategy"],
        vec![
            vec![
                text("p_b"),
                text("later"),
                text(&ts::format(at(30))),
                text("bottom_up"),
            ],
            vec![
                text("p_a"),
                text("first"),
                text(&ts::format(at(0))),
                text("top_down"),
            ],
            vec![
                text("p_c"),
                text("silent"),
                text(&ts::format(at(40))),
                text("bottom_up"),
            ],
        ],
    );
    t.set(
        "path_targets",
        &["path_id", "id"],
        vec![
            vec![text("p_a"), text("fnd.2.1.1")],
            vec![text("p_a"), text("fnd.2")],
            vec![text("p_b"), text("fnd.1")],
        ],
    );
    t.set(
        "path_subpath",
        &["path_id", "id"],
        vec![
            vec![text("p_a"), text("fnd.1.1.1")],
            vec![text("p_a"), text("fnd.2.1.1")],
        ],
    );
    t.set(
        "events",
        &[
            "ts", "kind", "path_id", "atom_id", "quiz_id", "rating", "payload",
        ],
        event_rows(),
    );
    t.set(
        "overlay_lessons",
        &["atom_id", "body"],
        vec![vec![text("fnd.9.9.9"), text("body")]],
    );
    t.set(
        "overlay_quizzes",
        &[
            "atom_id",
            "quiz_id",
            "difficulty",
            "kind",
            "question",
            "answer",
            "rubric",
        ],
        vec![vec![
            text("fnd.9.9.9"),
            text("fnd.9.9.9.q1"),
            text("medium"),
            text("multiple_choice"),
            text("q?"),
            text("a"),
            V::Null,
        ]],
    );
    // An authored quiz later removed, and a removed shipped quiz.
    t.set(
        "overlay_removed_quizzes",
        &["quiz_id", "atom_id"],
        vec![
            vec![text("fnd.2.1.1.q3"), V::Null],
            vec![text("fnd.9.9.9.q1"), text("fnd.9.9.9")],
        ],
    );
    t.set(
        "cards",
        &[
            "path_id",
            "quiz_id",
            "stability",
            "difficulty",
            "due_at",
            "last_reviewed_at",
            "reps",
            "lapses",
        ],
        card_rows(&legacy_cards()),
    );
    t
}

/// The cards the libSQL release kept for `p_a`'s answers.
fn legacy_cards() -> Vec<CardRow> {
    let a: Vec<Event> = events().into_iter().filter(|e| e.path == "p_a").collect();
    cards::fold_history("p_a", &a).unwrap().0
}

fn card_rows(cards: &[CardRow]) -> Vec<Vec<V>> {
    cards
        .iter()
        .filter_map(|c| {
            let s = c.state?;
            Some(vec![
                text(&c.path_id),
                text(&c.quiz_id),
                V::Float {
                    value: f64::from(s.stability),
                },
                V::Float {
                    value: f64::from(s.difficulty),
                },
                text(&ts::format(s.due)),
                text(&ts::format(s.last_review)),
                int(i64::from(c.reps)),
                int(i64::from(c.lapses)),
            ])
        })
        .collect()
}

async fn import(tables: Tables, db: &Db) -> mathtutor::Result<import::Report> {
    let url = serve(tables).await;
    import::import(&Turso::new(&url, Some(TOKEN.into())).unwrap(), db).await
}

// ── Tests ───────────────────────────────────────────────────────────

#[tokio::test]
async fn import_copies_paths_events_and_overlay_and_rebuilds_matching_cards() {
    let db = common::empty_db().await;
    let r = import(legacy(), &db).await.expect("import");
    assert_eq!(
        (r.paths, r.skipped, r.events, r.overlay_edits),
        (3, 0, 8, 4)
    );
    assert!(r.mismatches.is_empty(), "{:?}", r.mismatches);

    let want = Want {
        path: PathSel::None,
        overlay: true,
        all_paths: true,
    };
    let view = db.snapshot(&want).await.unwrap();
    let [a, b, c] = &view.paths[..] else {
        panic!("{} paths", view.paths.len());
    };
    assert_eq!((a.file.id.as_str(), a.file.goal.as_str()), ("p_a", "first"));
    assert_eq!(a.file.strategy, Strategy::TopDown);
    assert_eq!(a.file.targets, ["fnd.2.1.1", "fnd.2"]);
    assert_eq!(a.file.subpath, ["fnd.1.1.1", "fnd.2.1.1"]);
    assert_eq!(a.file.created_at, at(0));
    assert_eq!(
        a.file.updated_at,
        at(9),
        "activity is the latest event, not the import"
    );
    assert_eq!(
        (b.file.id.as_str(), b.file.strategy),
        ("p_b", Strategy::BottomUp)
    );
    assert_eq!(b.file.targets, ["fnd.1"]);
    assert!(b.file.subpath.is_empty());
    assert_eq!(b.file.updated_at, at(30));
    assert_eq!((c.file.id.as_str(), c.file.updated_at), ("p_c", at(40)));

    let stored = event_log::load(&db, "p_a").await.unwrap();
    let expected: Vec<Event> = events().into_iter().filter(|e| e.path == "p_a").collect();
    assert_eq!(stored.len(), expected.len());
    for (s, e) in stored.iter().zip(&expected) {
        assert_eq!(
            (s.ts, s.kind, &s.atom, &s.quiz),
            (e.ts, e.kind, &e.atom, &e.quiz)
        );
        assert_eq!(s.payload.rating, e.payload.rating);
        assert_eq!(s.payload.user_answer, e.payload.user_answer);
    }

    let authored = &view.overlay.atoms["fnd.9.9.9"];
    assert_eq!(authored.lesson.as_deref(), Some("body"));
    assert_eq!(authored.quizzes[0].id, "fnd.9.9.9.q1");
    assert!(
        authored.removed.contains("fnd.9.9.9.q1"),
        "a removed authored quiz stays removed"
    );
    assert!(
        view.overlay.atoms["fnd.2.1.1"]
            .removed
            .contains("fnd.2.1.1.q3")
    );

    for want in legacy_cards() {
        let got = &a.cards[&want.quiz_id];
        assert_eq!(
            (got.reps, got.lapses, got.presented),
            (want.reps, want.lapses, want.presented)
        );
        assert_eq!(got.state.map(|s| s.due), want.state.map(|s| s.due));
        assert_eq!(got.recent, want.recent);
    }
    assert_eq!(a.taught["fnd.2.1.1"].count, 1);
    assert!(b.cards.is_empty() && b.taught.is_empty());
}

#[tokio::test]
async fn rerunning_an_import_skips_imported_paths() {
    let db = common::empty_db().await;
    import(legacy(), &db).await.expect("first import");
    let r = import(legacy(), &db).await.expect("second import");
    assert_eq!((r.paths, r.skipped, r.events), (0, 3, 0));
    assert!(r.mismatches.is_empty(), "{:?}", r.mismatches);
    assert_eq!(event_log::load(&db, "p_a").await.unwrap().len(), 7);
}

#[tokio::test]
async fn import_reports_cards_that_differ_from_libsql() {
    let db = common::empty_db().await;
    let mut cards = legacy_cards();
    cards[0].reps += 1;
    let mut tables = legacy();
    tables.set(
        "cards",
        &[
            "path_id",
            "quiz_id",
            "stability",
            "difficulty",
            "due_at",
            "last_reviewed_at",
            "reps",
            "lapses",
        ],
        card_rows(&cards),
    );
    let r = import(tables, &db).await.expect("import");
    assert_eq!(r.mismatches.len(), 1, "{:?}", r.mismatches);
    assert!(r.mismatches[0].starts_with(&format!("p_a/{}:", cards[0].quiz_id)));
}

#[tokio::test]
async fn import_refuses_a_database_at_another_migration() {
    let db = common::empty_db().await;
    let mut tables = legacy();
    tables.set("schema_migrations", &["version"], vec![vec![int(3)]]);
    let e = import(tables, &db).await.unwrap_err();
    assert!(
        matches!(&e, Error::Turso(m) if m.contains("migration 3")),
        "{e}"
    );
    let want = Want {
        path: PathSel::None,
        overlay: true,
        all_paths: true,
    };
    let view = db.snapshot(&want).await.unwrap();
    assert!(view.paths.is_empty() && view.overlay.atoms.is_empty());
}

#[tokio::test]
async fn import_fails_when_turso_rejects_the_token() {
    let db = common::empty_db().await;
    let url = serve(legacy()).await;
    let e = import::import(&Turso::new(&url, Some("wrong".into())).unwrap(), &db)
        .await
        .unwrap_err();
    assert!(matches!(&e, Error::Turso(m) if m.starts_with("401")), "{e}");
}
