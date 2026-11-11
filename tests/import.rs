//! Integration tests for `mt import-turso` against a mock libSQL HTTP server.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use baserunna_client::bytes::Bytes;
use baserunna_client::{BoxFuture, HttpTransport, Transport, TransportError, http};
use chrono::{DateTime, Duration, Utc};
use mathtutor::Error;
use mathtutor::cards::{self, CardRow};
use mathtutor::db::{Db, Op, ts};
use mathtutor::event_log::{self, Event, EventKind};
use mathtutor::import::{self, Turso};
use mathtutor::overlay;
use mathtutor::schema::OAuthClient;
use mathtutor::types::{Rating, Strategy};
use mathtutor::view::Want;
use serde::{Deserialize, Serialize};

mod common;

const TOKEN: &str = "turso-token";

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
    Error { error: ResError },
}

#[derive(Serialize)]
struct ResError {
    message: String,
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
    rows: Vec<Vec<CellValue>>,
}

#[derive(Serialize, Clone)]
struct Col {
    name: String,
}

#[derive(Serialize, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
enum CellValue {
    Null,
    Integer { value: String },
    Float { value: f64 },
    Text { value: String },
}

fn text_val(s: &str) -> CellValue {
    CellValue::Text { value: s.into() }
}

fn opt_text_val(s: Option<&str>) -> CellValue {
    s.map_or(CellValue::Null, text_val)
}

fn int_val(n: i64) -> CellValue {
    CellValue::Integer {
        value: n.to_string(),
    }
}

#[derive(Default, Clone)]
struct Tables(HashMap<&'static str, StmtResult>, bool);

impl Tables {
    fn set(&mut self, table: &'static str, cols: &[&str], rows: Vec<Vec<CellValue>>) {
        let cols = cols.iter().map(|c| Col { name: (*c).into() }).collect();
        self.0.insert(table, StmtResult { cols, rows });
    }
}

/// libSQL snapshots served in order, with the final snapshot repeating.
struct Source {
    snapshots: Vec<Tables>,
    served: AtomicUsize,
}

async fn pipeline(
    State(source): State<Arc<Source>>,
    headers: HeaderMap,
    Json(req): Json<PipelineReq>,
) -> Result<Json<PipelineResp>, StatusCode> {
    let auth = headers.get("authorization").and_then(|v| v.to_str().ok());
    if auth != Some(&format!("Bearer {TOKEN}")) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let served = source.served.fetch_add(1, Ordering::Relaxed);
    let tables = &source.snapshots[served.min(source.snapshots.len() - 1)];
    let results = req
        .requests
        .into_iter()
        .map(|r| match r {
            Req::Execute { stmt } if tables.1 && stmt.sql == "BEGIN" => Res::Error {
                error: ResError {
                    message: "snapshot unavailable".into(),
                },
            },
            r => Res::Ok {
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
            },
        })
        .collect();
    Ok(Json(PipelineResp { results }))
}

async fn serve(tables: Tables) -> String {
    serve_snapshots(vec![tables]).await
}

async fn serve_snapshots(snapshots: Vec<Tables>) -> String {
    let source = Source {
        snapshots,
        served: AtomicUsize::new(0),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v2/pipeline", post(pipeline))
        .with_state(Arc::new(source));
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

fn at(day: i64) -> DateTime<Utc> {
    ts::parse("2026-01-01T08:30:00.250Z").unwrap() + Duration::days(day)
}

fn event(path: &str, day: i64, kind: EventKind) -> Event {
    Event {
        ts: at(day),
        path: path.into(),
        kind,
    }
}

fn taught(path: &str, day: i64, atom: &str) -> Event {
    let atom = atom.into();
    event(path, day, EventKind::LessonTaught { atom })
}

fn answer(path: &str, day: i64, quiz: &str, rating: Rating, user_answer: Option<&str>) -> Event {
    let answered = EventKind::QuizAnswered {
        atom: None,
        quiz: quiz.into(),
        rating,
        user_answer: user_answer.map(Into::into),
    };
    event(path, day, answered)
}

fn events() -> Vec<Event> {
    let presented = EventKind::QuizPresented {
        atom: "fnd.2.1.1".into(),
        quiz: "fnd.2.1.1.q1".into(),
    };
    vec![
        event("p_a", 0, EventKind::PathCreated {}),
        taught("p_a", 0, "fnd.2.1.1"),
        event("p_a", 0, presented),
        answer("p_a", 0, "fnd.2.1.1.q1", Rating::Good, Some("42")),
        answer("p_a", 3, "fnd.2.1.1.q1", Rating::Again, None),
        answer("p_a", 4, "fnd.2.1.1.q2", Rating::Easy, None),
        answer("p_a", 9, "fnd.2.1.1.q1", Rating::Hard, None),
        event("p_b", 30, EventKind::PathCreated {}),
    ]
}

fn event_rows() -> Vec<Vec<CellValue>> {
    rows_of_events(&events())
}

fn rows_of_events(events: &[Event]) -> Vec<Vec<CellValue>> {
    events
        .iter()
        .map(|e| {
            let fields = serde_json::to_value(&e.kind).unwrap();
            let text = |name: &str| fields.get(name).and_then(|v| v.as_str());
            let rating = fields
                .get("rating")
                .map(|r| serde_json::from_value::<Rating>(r.clone()).unwrap());
            let payload = text("user_answer").map(|a| format!(r#"{{"user_answer":"{a}"}}"#));
            vec![
                text_val(&ts::format(e.ts)),
                text_val(text("kind").unwrap()),
                text_val(&e.path),
                opt_text_val(text("atom")),
                opt_text_val(text("quiz")),
                rating.map_or(CellValue::Null, |r| int_val(i64::from(r))),
                opt_text_val(payload.as_deref()),
            ]
        })
        .collect()
}

fn legacy() -> Tables {
    let mut t = Tables::default();
    t.set("schema_migrations", &["version"], vec![vec![int_val(4)]]);
    t.set(
        "paths",
        &["id", "goal", "created_at", "strategy"],
        vec![
            vec![
                text_val("p_b"),
                text_val("later"),
                text_val(&ts::format(at(30))),
                text_val("bottom_up"),
            ],
            vec![
                text_val("p_a"),
                text_val("first"),
                text_val(&ts::format(at(0))),
                text_val("top_down"),
            ],
            vec![
                text_val("p_c"),
                text_val("silent"),
                text_val(&ts::format(at(40))),
                text_val("bottom_up"),
            ],
        ],
    );
    t.set(
        "path_targets",
        &["path_id", "id"],
        vec![
            vec![text_val("p_a"), text_val("fnd.2.1.1")],
            vec![text_val("p_a"), text_val("fnd.2")],
            vec![text_val("p_b"), text_val("fnd.1")],
        ],
    );
    t.set(
        "path_subpath",
        &["path_id", "id"],
        vec![
            vec![text_val("p_a"), text_val("fnd.1.1.1")],
            vec![text_val("p_a"), text_val("fnd.2.1.1")],
        ],
    );
    t.set("events", EVENT_COLUMNS, event_rows());
    t.set(
        "overlay_lessons",
        &["atom_id", "body"],
        vec![vec![text_val("fnd.9.9.9"), text_val("body")]],
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
            text_val("fnd.9.9.9"),
            text_val("fnd.9.9.9.q1"),
            text_val("medium"),
            text_val("multiple_choice"),
            text_val("q?"),
            text_val("a"),
            CellValue::Null,
        ]],
    );
    t.set(
        "overlay_removed_quizzes",
        &["quiz_id", "atom_id"],
        vec![
            vec![text_val("fnd.2.1.1.q3"), CellValue::Null],
            vec![text_val("fnd.9.9.9.q1"), text_val("fnd.9.9.9")],
        ],
    );
    t.set("cards", CARD_COLUMNS, card_rows(&legacy_cards()));
    t
}

fn legacy_cards() -> Vec<CardRow> {
    let a: Vec<Event> = events().into_iter().filter(|e| e.path == "p_a").collect();
    let projection = cards::Projection::from_events(&a).unwrap();
    let mut cards: Vec<CardRow> = projection.cards.into_values().collect();
    cards.sort_by(|x, y| x.quiz_id.cmp(&y.quiz_id));
    cards
}

const EVENT_COLUMNS: &[&str] = &[
    "ts", "kind", "path_id", "atom_id", "quiz_id", "rating", "payload",
];

const CARD_COLUMNS: &[&str] = &[
    "path_id",
    "quiz_id",
    "stability",
    "difficulty",
    "due_at",
    "last_reviewed_at",
    "reps",
    "lapses",
];

fn card_rows(cards: &[CardRow]) -> Vec<Vec<CellValue>> {
    cards
        .iter()
        .filter_map(|c| {
            let s = c.state?;
            Some(vec![
                text_val(&c.path_id),
                text_val(&c.quiz_id),
                CellValue::Float {
                    value: f64::from(s.stability),
                },
                CellValue::Float {
                    value: f64::from(s.difficulty),
                },
                text_val(&ts::format(s.due)),
                text_val(&ts::format(s.last_review)),
                int_val(i64::from(c.reps)),
                int_val(i64::from(c.lapses)),
            ])
        })
        .collect()
}

async fn import(tables: Tables, db: &Db) -> mathtutor::Result<import::Report> {
    let url = serve(tables).await;
    import::import(&Turso::new(&url, Some(TOKEN.into())).unwrap(), db).await
}

#[tokio::test]
async fn import_copies_paths_events_and_overlay_and_rebuilds_matching_cards() {
    let db = common::empty_db().await;
    let r = import(legacy(), &db).await.expect("import");
    assert_eq!(
        (r.paths, r.skipped, r.events, r.overlay_edits),
        (3, 0, 8, 4)
    );
    assert!(r.mismatches.is_empty(), "{:?}", r.mismatches);

    let view = db.snapshot(&Want::all_paths()).await.unwrap();
    let [a, b, c] = view.paths()[..] else {
        panic!("{} paths", view.paths().len());
    };
    assert_eq!((a.file.id.as_str(), a.file.goal.as_str()), ("p_a", "first"));
    assert_eq!(a.file.strategy, Strategy::TopDown);
    assert_eq!(a.file.targets, ["fnd.2.1.1", "fnd.2"]);
    assert_eq!(a.file.subpath, ["fnd.1.1.1", "fnd.2.1.1"]);
    assert_eq!(a.file.created_at, at(0));
    assert_eq!(
        a.updated_at,
        at(9),
        "activity is the latest event, not the import"
    );
    assert_eq!(
        (b.file.id.as_str(), b.file.strategy),
        ("p_b", Strategy::BottomUp)
    );
    assert_eq!(b.file.targets, ["fnd.1"]);
    assert_eq!(b.file.subpath, Vec::<String>::new());
    assert_eq!(b.updated_at, at(30));
    assert_eq!((c.file.id.as_str(), c.updated_at), ("p_c", at(40)));

    let stored = event_log::load(&db, "p_a").await.unwrap();
    let expected: Vec<Event> = events().into_iter().filter(|e| e.path == "p_a").collect();
    assert_eq!(stored, expected);

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
        let got = &a.projection.cards[&want.quiz_id];
        assert_eq!(
            (got.reps, got.lapses, got.presented),
            (want.reps, want.lapses, want.presented)
        );
        assert_eq!(got.state.map(|s| s.due), want.state.map(|s| s.due));
        assert_eq!(got.recent, want.recent);
    }
    assert_eq!(a.projection.taught["fnd.2.1.1"].count, 1);
    assert!(b.projection.cards.is_empty() && b.projection.taught.is_empty());
}

fn tables_with_wrong_card() -> Tables {
    let mut cards = legacy_cards();
    cards[0].reps += 1;
    let mut tables = legacy();
    tables.set("cards", CARD_COLUMNS, card_rows(&cards));
    tables
}

#[tokio::test]
async fn unfinished_import_resumes_and_skips_imported_paths() {
    let db = common::empty_db().await;
    let first = import(tables_with_wrong_card(), &db).await.expect("first");
    assert_eq!(first.mismatches.len(), 1, "{:?}", first.mismatches);

    let r = import(legacy(), &db).await.expect("resume");

    assert_eq!((r.paths, r.skipped, r.events), (0, 3, 0));
    assert!(r.mismatches.is_empty(), "{:?}", r.mismatches);
    assert_eq!(event_log::load(&db, "p_a").await.unwrap().len(), 7);
}

#[tokio::test]
async fn open_import_blocks_a_handle_opened_before_it_until_it_verifies() {
    let db = common::empty_db().await;
    import(tables_with_wrong_card(), &db).await.expect("first");

    let read = db.snapshot(&Want::overlay()).await.unwrap_err();
    assert!(matches!(read, Error::ImportInProgress), "{read}");
    let write = db
        .transact(&Want::overlay(), |_, changes| {
            overlay::upsert_lesson(changes, "fnd.1.1.1", "edit during the import");
            Ok(())
        })
        .await
        .unwrap_err();
    assert!(matches!(write, Error::ImportInProgress), "{write}");
    let recent = db.snapshot(&Want::path(None)).await.unwrap_err();
    let events = event_log::load(&db, "p_a").await.unwrap_err();
    let missing_stream = event_log::load(&db, "p_none").await.unwrap_err();
    for e in [recent, events, missing_stream] {
        assert!(matches!(e, Error::ImportInProgress), "{e}");
    }
    #[cfg(feature = "mcp")]
    {
        let token = mathtutor::mcp::oauth::validate_access_token(&db, "token").await;
        assert!(matches!(token, Err(Error::ImportInProgress)), "{token:?}");
    }

    let r = import(legacy(), &db).await.expect("resume");
    assert!(r.mismatches.is_empty(), "{:?}", r.mismatches);
    db.snapshot(&Want::overlay())
        .await
        .expect("a verified import allows commands");
}

#[tokio::test]
async fn completed_import_rejects_rerun_and_keeps_newer_content() {
    let db = common::empty_db().await;
    import(legacy(), &db).await.expect("first import");
    let atom = common::overlay(&db)
        .await
        .atoms
        .iter()
        .find(|(_, a)| a.lesson.is_some())
        .map(|(id, _)| id.clone())
        .expect("imported lesson");
    db.transact(&Want::overlay(), |_, changes| {
        overlay::upsert_lesson(changes, &atom, "newer");
        Ok(())
    })
    .await
    .unwrap();

    let e = import(legacy(), &db).await.unwrap_err();

    assert!(matches!(e, Error::ImportAlreadyComplete), "{e}");
    assert_eq!(
        common::overlay(&db).await.atoms[&atom].lesson.as_deref(),
        Some("newer")
    );
}

/// Transport that records the operations of every request.
struct RecordingTransport {
    inner: HttpTransport,
    requests: Arc<Mutex<Vec<Vec<serde_json::Value>>>>,
}

impl Transport for RecordingTransport {
    fn send(
        &self,
        req: http::Request<Bytes>,
    ) -> BoxFuture<'_, Result<http::Response<Bytes>, TransportError>> {
        let body: serde_json::Value = serde_json::from_slice(req.body()).unwrap_or_default();
        if let Some(ops) = body["ops"].as_array() {
            self.requests.lock().unwrap().push(ops.clone());
        }
        self.inner.send(req)
    }
}

#[tokio::test]
async fn import_writes_projections_with_their_path_and_reads_each_stream_once() {
    let cfg = common::fresh_config();
    common::create_db(&cfg).await;
    let requests = Arc::<Mutex<Vec<Vec<serde_json::Value>>>>::default();
    let transport = RecordingTransport {
        inner: cfg.transport().unwrap(),
        requests: requests.clone(),
    };
    let db = Db::with_transport(&cfg, transport);

    let r = import(legacy(), &db).await.expect("import");

    assert!(r.mismatches.is_empty(), "{:?}", r.mismatches);
    let requests = requests.lock().unwrap();
    let names = |op: &serde_json::Value, name: &str, field: &str, value: &str| {
        op["op"] == name && op[field] == value
    };
    let stream_reads = requests
        .iter()
        .flatten()
        .filter(|op| names(op, "read_stream", "stream", "events.p_a"))
        .count();
    assert_eq!(stream_reads, 1, "only the verification reads the stream");
    let write = requests
        .iter()
        .find(|ops| {
            ops.iter()
                .any(|op| names(op, "append", "stream", "events.p_a"))
        })
        .expect("the write of p_a");
    for table in ["paths", "cards", "taught"] {
        assert!(
            write
                .iter()
                .any(|op| op["table"] == table && op["op"] != "query_rows"),
            "the write of p_a writes {table}"
        );
    }
}

#[tokio::test]
async fn import_rejects_non_empty_target_database() {
    let db = common::fresh_db("p_existing").await;
    let e = import(legacy(), &db).await.unwrap_err();
    assert!(matches!(e, Error::ImportTargetNotEmpty), "{e}");
}

#[tokio::test]
async fn import_reports_overlay_and_path_fields_that_differ_from_libsql() {
    let db = common::empty_db().await;
    import(tables_with_wrong_card(), &db).await.expect("first");

    let mut changed = legacy();
    changed.set("overlay_lessons", &["atom_id", "body"], Vec::new());
    changed.set(
        "path_targets",
        &["path_id", "id"],
        vec![vec![text_val("p_a"), text_val("fnd.9")]],
    );
    let r = import(changed, &db).await.expect("resume");

    assert!(
        r.mismatches.iter().any(|m| m.starts_with("overlay/")),
        "{:?}",
        r.mismatches
    );
    assert!(
        r.mismatches
            .iter()
            .any(|m| m == "p_a: stored path differs from libSQL"),
        "{:?}",
        r.mismatches
    );
}

#[tokio::test]
async fn import_reports_an_event_whose_contents_differ_from_libsql() {
    let db = common::empty_db().await;
    import(tables_with_wrong_card(), &db).await.expect("first");
    let mut changed_events = events();
    let answered = changed_events.iter_mut().find_map(|e| match &mut e.kind {
        EventKind::QuizAnswered {
            user_answer: Some(text),
            ..
        } => Some((e.path.clone(), text)),
        _ => None,
    });
    let (path, text) = answered.expect("an event with an answer");
    *text = "a different answer".into();
    let mut changed = legacy();
    changed.set("events", EVENT_COLUMNS, rows_of_events(&changed_events));

    let r = import(changed, &db).await.expect("resume");

    assert!(
        r.mismatches
            .iter()
            .any(|m| m.starts_with(&format!("{path}:")) && m.contains("events")),
        "{:?}",
        r.mismatches
    );
}

#[tokio::test]
async fn import_reports_a_libsql_write_that_lands_after_its_first_read() {
    let db = common::empty_db().await;
    let mut late_events = events();
    late_events.push(taught("p_a", 90, "a"));
    let mut after_late_write = legacy();
    after_late_write.set("events", EVENT_COLUMNS, rows_of_events(&late_events));
    let url = serve_snapshots(vec![legacy(), after_late_write]).await;

    let r = import::import(&Turso::new(&url, Some(TOKEN.into())).unwrap(), &db)
        .await
        .expect("import");

    assert!(
        r.mismatches
            .iter()
            .any(|m| m.starts_with("p_a:") && m.contains("events")),
        "{:?}",
        r.mismatches
    );
    let blocked = db.snapshot(&Want::overlay()).await.unwrap_err();
    assert!(matches!(blocked, Error::ImportInProgress), "{blocked}");
}

#[tokio::test]
async fn malformed_history_fails_before_the_import_claims_the_target() {
    let db = common::empty_db().await;
    let mut rows = event_rows();
    let rating = EVENT_COLUMNS.iter().position(|c| *c == "rating").unwrap();
    let answer = rows
        .iter_mut()
        .find(|row| row[rating] != CellValue::Null)
        .expect("an answer");
    answer[rating] = CellValue::Null;
    let mut tables = legacy();
    tables.set("events", EVENT_COLUMNS, rows);

    let e = import(tables, &db).await.unwrap_err();

    assert!(
        matches!(&e, Error::BadRow(m) if m.contains("rating")),
        "{e}"
    );
    db.snapshot(&Want::overlay())
        .await
        .expect("the target stays unclaimed");
}

#[tokio::test]
async fn an_unknown_payload_field_fails_before_the_import_claims_the_target() {
    let db = common::empty_db().await;
    let mut rows = event_rows();
    let payload = EVENT_COLUMNS.iter().position(|c| *c == "payload").unwrap();
    rows[0][payload] = text_val(r#"{"hint":"kept nowhere"}"#);
    let mut tables = legacy();
    tables.set("events", EVENT_COLUMNS, rows);

    let e = import(tables, &db).await.unwrap_err();

    assert!(matches!(&e, Error::BadRow(m) if m.contains("hint")), "{e}");
    db.snapshot(&Want::overlay())
        .await
        .expect("the target stays unclaimed");
}

#[tokio::test]
async fn import_reports_source_rows_that_it_did_not_store() {
    let db = common::empty_db().await;
    let mut orphans = events();
    orphans.push(taught("p_gone", 1, "fnd.2.1.1"));
    let mut tables = legacy();
    tables.set("events", EVENT_COLUMNS, rows_of_events(&orphans));
    tables.set(
        "path_targets",
        &["path_id", "id"],
        vec![
            vec![text_val("p_a"), text_val("fnd.2.1.1")],
            vec![text_val("p_a"), text_val("fnd.2")],
            vec![text_val("p_b"), text_val("fnd.1")],
            vec![text_val("p_gone"), text_val("fnd.1")],
        ],
    );

    let r = import(tables, &db).await.expect("import");

    assert_eq!(
        r.mismatches,
        [
            "events: 9 rows in libSQL, but the import stored 8",
            "path_targets: 4 rows in libSQL, but the import stored 3",
        ]
    );
    let blocked = db.snapshot(&Want::overlay()).await.unwrap_err();
    assert!(matches!(blocked, Error::ImportInProgress), "{blocked}");
}

#[tokio::test]
async fn repeated_import_reports_a_late_event_on_a_path_without_a_stream() {
    let db = common::empty_db().await;
    let mut late_events = events();
    late_events.push(taught("p_c", 50, "a"));
    let mut with_late_event = legacy();
    with_late_event.set("events", EVENT_COLUMNS, rows_of_events(&late_events));
    let url = serve_snapshots(vec![legacy(), with_late_event]).await;
    let turso = Turso::new(&url, Some(TOKEN.into())).unwrap();
    import::import(&turso, &db).await.expect("first");

    let r = import::import(&turso, &db).await.expect("repeat");

    assert!(
        r.mismatches.iter().any(|m| m.starts_with("p_c:")),
        "{:?}",
        r.mismatches
    );
}

#[tokio::test]
async fn import_fails_if_libsql_snapshot_cannot_begin() {
    let db = common::empty_db().await;
    let mut tables = legacy();
    tables.1 = true;

    let e = import(tables, &db).await.unwrap_err();

    assert!(
        matches!(&e, Error::Turso(m) if m.starts_with("BEGIN")),
        "{e}"
    );
}

#[tokio::test]
async fn import_reports_a_libsql_path_that_lands_after_its_first_read() {
    let db = common::empty_db().await;
    let mut with_late_path = legacy();
    let late_path = vec![
        text_val("p_late"),
        text_val("late"),
        text_val(&ts::format(at(40))),
        text_val("bottom_up"),
    ];
    with_late_path.set(
        "paths",
        &["id", "goal", "created_at", "strategy"],
        vec![late_path],
    );
    let url = serve_snapshots(vec![legacy(), with_late_path]).await;

    let r = import::import(&Turso::new(&url, Some(TOKEN.into())).unwrap(), &db)
        .await
        .expect("import reports the late path as a difference");

    assert!(
        r.mismatches
            .iter()
            .any(|m| m == "p_late: path missing from Baserunna"),
        "{:?}",
        r.mismatches
    );
}

#[tokio::test]
async fn import_copies_oauth_clients_so_connectors_stay_registered() {
    let cfg = common::fresh_config();
    let db = common::create_db(&cfg).await;
    let mut tables = legacy();
    tables.set(
        "oauth_clients",
        &["client_id", "client_name", "redirect_uris", "created_at"],
        vec![vec![
            text_val("client-1"),
            text_val("Claude"),
            text_val(r#"["https://claude.ai/callback"]"#),
            text_val(&ts::format(at(1))),
        ]],
    );

    let r = import(tables, &db).await.expect("import");

    assert_eq!(r.oauth_clients, 1);
    let stored = db
        .client()
        .db(&cfg.db)
        .read_one(Op::get_row::<OAuthClient>("client-1"))
        .await
        .unwrap()
        .row::<OAuthClient>()
        .unwrap()
        .expect("stored client");
    assert_eq!(stored.redirect_uris, ["https://claude.ai/callback"]);
}

#[tokio::test]
async fn import_reports_cards_that_differ_from_libsql() {
    let db = common::empty_db().await;
    let r = import(tables_with_wrong_card(), &db).await.expect("import");
    assert_eq!(r.mismatches.len(), 1, "{:?}", r.mismatches);
    assert!(r.mismatches[0].starts_with(&format!("p_a/{}:", legacy_cards()[0].quiz_id)));
}

#[tokio::test]
async fn import_rejects_database_at_another_migration() {
    let db = common::empty_db().await;
    let mut tables = legacy();
    tables.set("schema_migrations", &["version"], vec![vec![int_val(3)]]);
    let e = import(tables, &db).await.unwrap_err();
    assert!(
        matches!(&e, Error::Turso(m) if m.contains("migration 3")),
        "{e}"
    );
    let view = db.snapshot(&Want::all_paths()).await.unwrap();
    assert!(view.paths().is_empty() && view.overlay.atoms.is_empty());
}

#[tokio::test]
async fn import_fails_if_turso_rejects_token() {
    let db = common::empty_db().await;
    let url = serve(legacy()).await;
    let e = import::import(&Turso::new(&url, Some("wrong".into())).unwrap(), &db)
        .await
        .unwrap_err();
    assert!(matches!(&e, Error::Turso(m) if m.starts_with("401")), "{e}");
}
