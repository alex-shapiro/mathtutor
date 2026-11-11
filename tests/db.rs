//! Integration tests for database initialization, streams, concurrency, and caching.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use baserunna_client::api::{ColumnType, ListTablesQuery, NewColumn, NewTable};
use baserunna_client::bytes::Bytes;
use baserunna_client::{BoxFuture, Client, HttpTransport, Op, Transport, TransportError, http};
use chrono::{Duration, Utc};
use mathtutor::db::{Db, DbConfig};
use mathtutor::event_log::{self, Event, EventKind};
use mathtutor::graph::Curriculum;
use mathtutor::overlay::QuizDraft;
use mathtutor::path::PathFile;
use mathtutor::schema::TableSet;
use mathtutor::types::{Difficulty, QuizType, Rating, Strategy};
use mathtutor::view::Want;
use mathtutor::{Error, answer, cards, overlay, path, store};

mod common;

use common::PATH_ID;

const ATOM: &str = "fnd.2.1.1";

const TABLES: [&str; 6] = [
    "cards",
    "lessons",
    "paths",
    "quizzes",
    "removed_quizzes",
    "taught",
];

async fn table_names(cfg: &DbConfig) -> Vec<String> {
    let client = Client::new(cfg.transport().unwrap()).with_token(&cfg.token);
    let page = client
        .db(&cfg.db)
        .list_tables(ListTablesQuery::default())
        .await;
    let mut names: Vec<String> = page.unwrap().tables.into_iter().map(|t| t.name).collect();
    names.sort();
    names
}

#[tokio::test]
async fn create_if_missing_creates_the_database_and_every_table_once() {
    let cfg = common::fresh_config();
    let db = Db::connect(&cfg).unwrap();
    assert!(db.create_if_missing().await.unwrap(), "first call creates");
    assert_eq!(table_names(&cfg).await, TABLES);
    assert!(
        !db.create_if_missing().await.unwrap(),
        "second call finds it"
    );
    let fresh = Db::connect(&cfg).unwrap();
    fresh.snapshot(&Want::path(None)).await.expect("read");
}

/// Return the Baserunna client of the database of `cfg`.
fn raw_db(cfg: &DbConfig) -> baserunna_client::Db {
    Client::new(cfg.transport().unwrap())
        .with_token(&cfg.token)
        .db(&cfg.db)
}

#[tokio::test]
async fn setup_completes_after_an_interrupted_first_setup() {
    let cfg = common::fresh_config();
    let raw = raw_db(&cfg);
    raw.ensure().await.unwrap();
    for t in &TableSet::Learner.tables()[..3] {
        raw.create_table(t).await.unwrap();
    }
    let db = Db::connect(&cfg).unwrap();

    db.create_if_missing().await.unwrap();

    assert_eq!(table_names(&cfg).await, TABLES);
    common::seed_path(&db, PATH_ID, &[], Strategy::BottomUp).await;
    db.snapshot(&Want::path(None)).await.expect("read");
}

#[tokio::test]
async fn setup_rejects_an_existing_table_with_other_columns() {
    let cfg = common::fresh_config();
    let raw = raw_db(&cfg);
    raw.ensure().await.unwrap();
    let paths = NewTable {
        name: "paths".into(),
        columns: vec![NewColumn::new("id", ColumnType::String)],
        primary_key: vec!["id".into()],
        indexes: Vec::new(),
        if_not_exists: false,
    };
    raw.create_table(&paths).await.unwrap();

    let e = Db::connect(&cfg)
        .unwrap()
        .create_if_missing()
        .await
        .unwrap_err();

    assert!(
        matches!(&e, Error::TableMismatch { table, .. } if table == "paths"),
        "{e:?}"
    );
    assert!(e.to_string().contains("BASERUNNA_DB"), "{e}");
}

#[test]
fn debug_output_of_the_config_hides_the_token() {
    let cfg = common::fresh_config();

    let shown = format!("{cfg:?}");

    assert!(!shown.contains(&cfg.token), "{shown}");
    assert!(shown.contains(&cfg.db), "{shown}");
}

/// Assert that `e` reports the missing database of `cfg`.
fn assert_database_not_found(e: &Error, cfg: &DbConfig) {
    let message = e.to_string();
    assert!(matches!(e, Error::DatabaseNotFound { .. }), "{e:?}");
    assert!(message.contains(&format!("'{}'", cfg.db)), "{message}");
    assert!(message.contains(&cfg.url), "{message}");
    assert!(message.contains("BASERUNNA_DB"), "{message}");
    assert!(message.contains("BASERUNNA_URL"), "{message}");
    assert!(!message.contains("mt path new"), "{message}");
    assert_eq!(e.exit_code(), 2);
}

#[tokio::test]
async fn commands_reject_a_missing_database_without_creating_it() {
    let cfg = common::fresh_config();
    let db = Db::connect(&cfg).unwrap();

    let recent = db.snapshot(&Want::path(None)).await.unwrap_err();
    let explicit = path::cmd_path_strategy(&db, Some(PATH_ID), Strategy::TopDown)
        .await
        .unwrap_err();
    let events = event_log::load(&db, PATH_ID).await.unwrap_err();

    for e in [recent, explicit, events] {
        assert_database_not_found(&e, &cfg);
    }
    let client = Client::new(cfg.transport().unwrap()).with_token(&cfg.token);
    let info = client.db(&cfg.db).info().await;
    assert!(
        info.is_err(),
        "a command must not create the database: {info:?}"
    );
}

#[tokio::test]
async fn commands_reject_a_database_without_math_tutor_tables() {
    let cfg = common::fresh_config();
    let client = Client::new(cfg.transport().unwrap()).with_token(&cfg.token);
    client.db(&cfg.db).ensure().await.unwrap();
    let db = Db::connect(&cfg).unwrap();

    let e = db.snapshot(&Want::path(Some(PATH_ID))).await.unwrap_err();

    assert_database_not_found(&e, &cfg);
}

#[tokio::test]
async fn learner_commands_ignore_the_authorization_schema_version() {
    let cfg = common::fresh_config();
    let db = common::create_db(&cfg).await;
    let raw = raw_db(&cfg);
    raw.write(&[Op::put("mathtutor_auth_state", r#"{"schema":"99"}"#)])
        .await
        .unwrap();

    common::seed_path(&db, PATH_ID, &[], Strategy::BottomUp).await;
    db.snapshot(&Want::path(Some(PATH_ID))).await.unwrap();

    let e = db.with_auth_tables().await.unwrap_err();
    assert!(
        matches!(&e, Error::SchemaMismatch { tables: TableSet::Auth, found, .. } if found == "99"),
        "{e:?}"
    );
}

#[tokio::test]
async fn the_server_handle_creates_and_checks_the_authorization_tables() {
    let cfg = common::fresh_config();
    let db = common::create_db(&cfg).await;
    assert_eq!(table_names(&cfg).await, TABLES);

    let server = db.with_auth_tables().await.unwrap();
    let mut names = table_names(&cfg).await;
    names.retain(|n| !TABLES.contains(&n.as_str()));
    let mut auth: Vec<String> = TableSet::Auth
        .tables()
        .into_iter()
        .map(|t| t.name)
        .collect();
    auth.sort();
    assert_eq!(names, auth);
    server.snapshot(&Want::overlay()).await.unwrap();

    raw_db(&cfg)
        .write(&[Op::put("mathtutor_auth_state", r#"{"schema":"99"}"#)])
        .await
        .unwrap();
    let e = server.snapshot(&Want::overlay()).await.unwrap_err();
    assert!(
        matches!(
            &e,
            Error::SchemaMismatch {
                tables: TableSet::Auth,
                ..
            }
        ),
        "{e:?}"
    );
    db.snapshot(&Want::overlay()).await.unwrap();
}

#[tokio::test]
async fn a_handle_checks_the_database_state_in_every_request() {
    let cfg = common::fresh_config();
    let db = common::create_db(&cfg).await;
    common::seed_path(&db, PATH_ID, &[], Strategy::BottomUp).await;
    let want = Want::path(Some(PATH_ID));
    db.snapshot(&want).await.unwrap();
    let raw = raw_db(&cfg);
    let state = raw.read_one(Op::get("mathtutor_state")).await.unwrap();
    let state = state
        .entry()
        .unwrap()
        .unwrap()
        .value
        .data()
        .cloned()
        .unwrap();
    let mut open_import: serde_json::Value = serde_json::from_slice(&state).unwrap();
    open_import["import"] = "open".into();

    raw.write(&[Op::put("mathtutor_state", r#"{"schema":"99"}"#)])
        .await
        .unwrap();
    let e = db.snapshot(&want).await.unwrap_err();
    assert!(
        matches!(&e, Error::SchemaMismatch { found, .. } if found == "99"),
        "{e:?}"
    );

    raw.write(&[Op::put("mathtutor_state", open_import.to_string())])
        .await
        .unwrap();
    let e = db.snapshot(&want).await.unwrap_err();
    assert!(matches!(e, Error::ImportInProgress), "{e:?}");

    raw.delete().await.unwrap();
    raw.ensure().await.unwrap();
    let e = db.snapshot(&want).await.unwrap_err();
    assert_database_not_found(&e, &cfg);
}

#[tokio::test]
async fn commands_check_the_database_in_their_first_request() {
    let cfg = common::fresh_config();
    let seeded = common::create_db(&cfg).await;
    common::seed_path(&seeded, PATH_ID, &[], Strategy::BottomUp).await;

    let (db, traffic) = counting_db(&cfg);
    path::cmd_path_strategy(&db, Some(PATH_ID), Strategy::TopDown)
        .await
        .unwrap();
    assert_eq!(
        traffic.requests.load(Ordering::Relaxed),
        2,
        "a write to a named path reads and commits"
    );

    let (db, traffic) = counting_db(&cfg);
    path::cmd_path_strategy(&db, None, Strategy::BottomUp)
        .await
        .unwrap();
    assert_eq!(
        traffic.requests.load(Ordering::Relaxed),
        3,
        "a write to the most recent path scans activity, reads, and commits"
    );

    let (db, traffic) = counting_db(&cfg);
    db.snapshot(&Want::path(Some(PATH_ID))).await.unwrap();
    assert_eq!(
        traffic.requests.load(Ordering::Relaxed),
        1,
        "a read with a cold cache loads the overlay in the request that reads the path"
    );
}

#[tokio::test]
async fn most_recent_path_follows_logged_events() {
    let cfg = common::fresh_config();
    let db = common::create_db(&cfg).await;
    common::seed_path(&db, "p_a", &["fnd.1.1.1"], Strategy::BottomUp).await;
    common::seed_path(&db, "p_b", &["fnd.1.1.1"], Strategy::BottomUp).await;
    let recent = |db: &Db| {
        let db = db.clone();
        async move {
            let view = db
                .snapshot(&mathtutor::view::Want::path(None))
                .await
                .unwrap();
            let p = view.path().unwrap();
            (p.file.id.clone(), p.updated_at)
        }
    };
    assert_eq!(recent(&db).await.0, "p_b", "the last created path");
    store::cmd_lesson_upsert(&db, "fnd.1.1.1", "body".into(), Some("p_a"))
        .await
        .unwrap();
    let (id, at) = recent(&db).await;
    assert_eq!(id, "p_a");
    assert!(at > Utc::now() - Duration::minutes(1), "activity time {at}");
    let fresh = Db::connect(&cfg).unwrap();
    assert_eq!(recent(&fresh).await.0, "p_a");
    store::cmd_lesson_upsert(&db, "fnd.1.1.1", "again".into(), Some("p_b"))
        .await
        .unwrap();
    assert_eq!(
        recent(&db).await.0,
        "p_b",
        "the selection follows the newer activity"
    );
}

#[tokio::test]
async fn most_recent_path_follows_commit_order_instead_of_the_activity_time() {
    let db = common::empty_db().await;
    let ahead = PathFile {
        id: "p_clock_ahead".into(),
        ..common::path_with(&[])
    };
    let created = Event::now(&ahead.id, EventKind::PathCreated {});
    let future = mathtutor::db::ts::parse("2099-01-01T00:00:00.000Z").unwrap();
    db.transact(&Want::nothing(), |view, changes| {
        changes.create_path_with_history(view, &ahead, std::slice::from_ref(&created), future)
    })
    .await
    .unwrap();
    common::seed_path(&db, "p_later", &[], Strategy::BottomUp).await;

    let view = db.snapshot(&Want::path(None)).await.unwrap();

    assert_eq!(view.path().unwrap().file.id, "p_later");
    let all = db.snapshot(&Want::all_paths()).await.unwrap();
    let shown = all
        .paths()
        .iter()
        .find(|p| p.file.id == ahead.id)
        .unwrap()
        .updated_at;
    assert_eq!(shown, future, "the activity time stays for display");
}

#[tokio::test]
async fn opening_an_existing_database_preserves_data() {
    let cfg = common::fresh_config();
    let db = common::create_db(&cfg).await;
    common::seed_path(&db, PATH_ID, &["fnd.2"], Strategy::TopDown).await;
    common::write(&db, PATH_ID, |_, changes| {
        overlay::upsert_lesson(changes, ATOM, "kept");
    })
    .await;

    let db = Db::connect(&cfg).unwrap();
    assert_eq!(table_names(&cfg).await, TABLES);
    let ps = common::path_state(&db, PATH_ID).await;
    assert_eq!(ps.file.goal, "test goal");
    assert_eq!(ps.file.targets, ["fnd.2"]);
    assert_eq!(ps.file.strategy, Strategy::TopDown);
    let ov = common::overlay(&db).await;
    assert_eq!(ov.atoms[ATOM].lesson.as_deref(), Some("kept"));
}

#[tokio::test]
async fn events_round_trip_in_order_with_every_field() {
    let db = common::fresh_db(PATH_ID).await;
    let t0 = mathtutor::db::ts::parse("2026-05-26T10:00:00.123Z").unwrap();
    let full = Event {
        ts: t0,
        path: PATH_ID.into(),
        kind: EventKind::QuizAnswered {
            atom: Some("a".into()),
            quiz: "a.q1".into(),
            rating: Rating::Hard,
            user_answer: Some("why \"quoted\" ✓\nmulti\nline".into()),
        },
    };
    let bare = common::at(
        t0,
        Event::now(PATH_ID, EventKind::LessonTaught { atom: "b".into() }),
    );
    let removed = EventKind::QuizRemoved {
        atom: "a".into(),
        quiz: "a.q1".into(),
    };
    let removed = common::at(t0, Event::now(PATH_ID, removed));
    common::append(&db, &[full.clone(), bare.clone()]).await;
    common::append(&db, std::slice::from_ref(&removed)).await;

    let events = event_log::load(&db, PATH_ID).await.unwrap();
    assert_eq!(events[0].kind, EventKind::PathCreated {});
    assert_eq!(events[1..], [full, bare, removed]);
}

#[tokio::test]
async fn event_log_of_a_path_without_a_stream_is_empty() {
    let db = common::empty_db().await;

    assert_eq!(event_log::load(&db, "p_none").await.unwrap(), []);
}

#[tokio::test]
async fn event_log_pages_across_multiple_reads() {
    let db = common::fresh_db(PATH_ID).await;
    let ts = mathtutor::db::ts::parse("2026-05-26T10:00:00.123Z").unwrap();
    let events: Vec<Event> = (0..1205)
        .map(|i| common::at(ts, common::presented(&format!("q{i}"))))
        .collect();
    for chunk in events.chunks(500) {
        common::append(&db, chunk).await;
    }
    let loaded = event_log::load(&db, PATH_ID).await.unwrap();
    assert_eq!(loaded.len(), 1206);
    assert_eq!(loaded[1..], events);
}

#[tokio::test]
async fn concurrent_answers_are_recorded_once_each() {
    let db = common::fresh_db(PATH_ID).await;
    let quiz = store::cmd_quiz_create(
        &db,
        ATOM,
        QuizDraft {
            difficulty: Difficulty::Easy,
            question: "q?".into(),
            answer: "a".into(),
            rubric: None,
            kind: QuizType::FreeText,
        },
        Some(PATH_ID),
    )
    .await
    .unwrap();

    let tasks: Vec<_> = (0..8)
        .map(|i| {
            let db = db.clone();
            let quiz = quiz.clone();
            tokio::spawn(async move {
                let rating = if i % 2 == 0 {
                    Rating::Good
                } else {
                    Rating::Again
                };
                answer::cmd_quiz_answer(&db, &quiz, rating, None, Some(PATH_ID)).await
            })
        })
        .collect();
    for t in tasks {
        t.await.unwrap().expect("answer");
    }

    let ps = common::path_state(&db, PATH_ID).await;
    let card = &ps.projection.cards[&quiz];
    assert_eq!(card.reps, 8);
    assert_eq!(card.lapses, 4);
    assert_eq!(card.recent.len(), 8);
    let answered = event_log::load(&db, PATH_ID)
        .await
        .unwrap()
        .iter()
        .filter(|e| matches!(e.kind, EventKind::QuizAnswered { .. }))
        .count();
    assert_eq!(answered, 8);
}

#[tokio::test]
async fn append_rejects_a_path_outside_the_view_and_keeps_its_history() {
    let db = common::fresh_db(PATH_ID).await;
    common::append(&db, &[common::answered("a.q1", Rating::Good)]).await;

    let e = db
        .transact(&Want::overlay(), |view, changes| {
            changes.append(view, &common::answered("a.q1", Rating::Again))
        })
        .await
        .unwrap_err();

    assert!(
        matches!(&e, Error::PathNotLoaded(id) if id == PATH_ID),
        "{e:?}"
    );
    let card = &common::path_state(&db, PATH_ID).await.projection.cards["a.q1"];
    assert_eq!((card.reps, card.lapses), (1, 0));
}

#[tokio::test]
async fn append_accepts_events_after_the_path_created_event_in_one_write() {
    let db = common::empty_db().await;
    let created = Event::now("p_new", EventKind::PathCreated {});
    let taught = Event::now("p_new", EventKind::LessonTaught { atom: "a".into() });

    db.transact(&Want::nothing(), |view, changes| {
        changes.append(view, &created)?;
        changes.append(view, &taught)
    })
    .await
    .unwrap();

    let events = event_log::load(&db, "p_new").await.unwrap();
    assert_eq!(events.len(), 2);
}

#[tokio::test]
async fn a_new_path_is_written_with_its_full_history_and_activity_time() {
    let db = common::empty_db().await;
    let t0 = mathtutor::db::ts::parse("2026-05-26T10:00:00.123Z").unwrap();
    let p = PathFile {
        id: "p_history".into(),
        ..common::path_with(&[])
    };
    let history: Vec<Event> = (0..1205)
        .map(|i| Event {
            path: p.id.clone(),
            ..common::at(t0, common::presented(&format!("a.q{i}")))
        })
        .collect();
    let active_at = t0 + Duration::days(1);

    db.transact(&Want::nothing(), |view, changes| {
        changes.create_path_with_history(view, &p, &history, active_at)
    })
    .await
    .unwrap();

    assert_eq!(event_log::load(&db, &p.id).await.unwrap(), history);
    let state = common::path_state(&db, &p.id).await;
    assert_eq!(state.updated_at, active_at);
    assert_eq!(state.projection.cards.len(), history.len());
}

#[tokio::test]
async fn a_new_path_rejects_history_of_another_path() {
    let db = common::empty_db().await;
    let p = PathFile {
        id: "p_history".into(),
        ..common::path_with(&[])
    };
    let other = common::presented("a.q1");

    let e = db
        .transact(&Want::nothing(), |view, changes| {
            changes.create_path_with_history(view, &p, std::slice::from_ref(&other), Utc::now())
        })
        .await
        .unwrap_err();

    assert!(
        matches!(&e, Error::PathNotLoaded(id) if id == PATH_ID),
        "{e:?}"
    );
    let e = db.snapshot(&Want::path(Some(&p.id))).await.unwrap();
    assert!(e.path().is_err(), "the failed write set stores nothing");
}

#[tokio::test]
async fn replayed_history_reproduces_incremental_rows_exactly() {
    let db = common::fresh_db(PATH_ID).await;
    let t0 = Utc::now() - Duration::days(30);
    common::append(
        &db,
        &[
            common::at(t0, common::taught("a")),
            common::lesson_authored("b"),
            common::at(t0, common::presented("a.q1")),
            common::at(t0, common::answered("a.q1", Rating::Again)),
        ],
    )
    .await;
    common::append(
        &db,
        &[
            common::at(t0 + Duration::days(1), common::taught("a")),
            common::at(t0 + Duration::days(2), common::presented("a.q1")),
            common::at(
                t0 + Duration::days(2),
                common::answered("a.q1", Rating::Good),
            ),
            common::at(
                t0 + Duration::days(3),
                common::answered("a.q2", Rating::Easy),
            ),
        ],
    )
    .await;

    let before = common::path_state(&db, PATH_ID).await;
    let events = event_log::load(&db, PATH_ID).await.unwrap();
    let after = cards::Projection::from_events(&events).unwrap();
    let before = before.projection;

    assert_eq!(before.taught, after.taught);
    assert_eq!(after.taught["a"].count, 2);
    assert_eq!(after.taught["b"].count, 0);
    let mut quizzes: Vec<&String> = after.cards.keys().collect();
    quizzes.sort();
    assert_eq!(quizzes, ["a.q1", "a.q2"]);
    for (q, b) in &before.cards {
        let a = &after.cards[q];
        assert_eq!(
            (b.reps, b.lapses, b.presented),
            (a.reps, a.lapses, a.presented),
            "{q}"
        );
        assert_eq!(b.last_presented_at, a.last_presented_at, "{q}");
        assert_eq!(b.first_correct_at, a.first_correct_at, "{q}");
        assert_eq!(b.recent, a.recent, "{q}");
        let (bs, as_) = (b.state.unwrap(), a.state.unwrap());
        assert_eq!(bs.due, as_.due, "{q}");
        assert_eq!(bs.last_review, as_.last_review, "{q}");
        assert!((bs.stability - as_.stability).abs() < 1e-6, "{q}");
        assert!((bs.difficulty - as_.difficulty).abs() < 1e-6, "{q}");
    }
    let q1 = &after.cards["a.q1"];
    assert_eq!((q1.reps, q1.lapses, q1.presented), (2, 1, 2));
    assert_eq!(q1.recent, [Rating::Good, Rating::Again]);
}

/// Requests and response bytes that a [`CountingTransport`] observes.
#[derive(Default)]
struct Traffic {
    requests: AtomicUsize,
    received: AtomicUsize,
    /// Operations sent in interactive transaction requests.
    txn_ops: Mutex<Vec<serde_json::Value>>,
}

impl Traffic {
    /// Return the tables that interactive transactions queried.
    fn txn_queried_tables(&self) -> Vec<String> {
        let ops = self.txn_ops.lock().unwrap();
        ops.iter()
            .filter(|op| op["op"] == "query_rows")
            .filter_map(|op| op["table"].as_str().map(str::to_string))
            .collect()
    }
}

/// Transport that counts requests and response bytes.
struct CountingTransport {
    inner: HttpTransport,
    traffic: Arc<Traffic>,
}

impl Transport for CountingTransport {
    fn send(
        &self,
        req: http::Request<Bytes>,
    ) -> BoxFuture<'_, Result<http::Response<Bytes>, TransportError>> {
        Box::pin(async move {
            self.traffic.requests.fetch_add(1, Ordering::Relaxed);
            if req.uri().path().contains("/transactions") {
                let body: serde_json::Value =
                    serde_json::from_slice(req.body()).unwrap_or_default();
                if let Some(ops) = body["ops"].as_array() {
                    self.traffic
                        .txn_ops
                        .lock()
                        .unwrap()
                        .extend(ops.iter().cloned());
                }
            }
            let resp = self.inner.send(req).await?;
            self.traffic
                .received
                .fetch_add(resp.body().len(), Ordering::Relaxed);
            Ok(resp)
        })
    }
}

fn counting_db(cfg: &DbConfig) -> (Db, Arc<Traffic>) {
    let traffic = Arc::<Traffic>::default();
    let t = CountingTransport {
        inner: cfg.transport().unwrap(),
        traffic: traffic.clone(),
    };
    (Db::with_transport(cfg, t), traffic)
}

#[tokio::test]
async fn cached_overlay_is_reused_until_another_client_writes() {
    let cfg = common::fresh_config();
    let writer = common::create_db(&cfg).await;
    common::seed_path(&writer, PATH_ID, &[], Strategy::BottomUp).await;
    let body = "x".repeat(20_000);
    common::write(&writer, PATH_ID, |_, c| {
        overlay::upsert_lesson(c, "fnd.1.1.1", &body);
    })
    .await;

    let (reader, traffic) = counting_db(&cfg);
    let read = |db: &Db| {
        let db = db.clone();
        async move {
            db.snapshot(&mathtutor::view::Want::path(Some(PATH_ID)))
                .await
                .unwrap()
        }
    };
    let first = read(&reader).await;
    assert_eq!(
        first.overlay().unwrap().atoms["fnd.1.1.1"]
            .lesson
            .as_deref(),
        Some(body.as_str())
    );
    let cold = traffic.received.swap(0, Ordering::Relaxed);
    assert!(
        cold > 20_000,
        "first read downloads the overlay ({cold} bytes)"
    );

    let second = read(&reader).await;
    assert_eq!(
        second.overlay().unwrap().atoms["fnd.1.1.1"]
            .lesson
            .as_deref(),
        Some(body.as_str())
    );
    let warm = traffic.received.swap(0, Ordering::Relaxed);
    assert!(
        warm < 2_000,
        "a valid cache skips the overlay ({warm} bytes)"
    );

    common::write(&writer, PATH_ID, |_, c| {
        overlay::upsert_lesson(c, "fnd.1.1.1", "revised");
    })
    .await;
    let third = read(&reader).await;
    assert_eq!(
        third.overlay().unwrap().atoms["fnd.1.1.1"]
            .lesson
            .as_deref(),
        Some("revised")
    );
}

#[tokio::test]
async fn write_with_a_cold_cache_reads_overlay_rows_outside_the_transaction() {
    let cfg = common::fresh_config();
    let writer = common::create_db(&cfg).await;
    common::seed_path(&writer, PATH_ID, &[], Strategy::BottomUp).await;
    store::cmd_lesson_upsert(&writer, "fnd.1.1.1", "first".into(), Some(PATH_ID))
        .await
        .unwrap();

    let (db, traffic) = counting_db(&cfg);
    store::cmd_lesson_upsert(&db, "fnd.1.1.2", "second".into(), Some(PATH_ID))
        .await
        .unwrap();

    let queried = traffic.txn_queried_tables();
    assert!(!queried.is_empty(), "the transaction reads the path");
    for table in ["lessons", "quizzes", "removed_quizzes"] {
        assert!(
            !queried.iter().any(|t| t == table),
            "{table} in {queried:?}"
        );
    }
    let view = db.snapshot(&Want::overlay()).await.unwrap();
    assert_eq!(
        view.overlay().unwrap().atoms["fnd.1.1.1"].lesson.as_deref(),
        Some("first")
    );
    assert_eq!(
        view.overlay().unwrap().atoms["fnd.1.1.2"].lesson.as_deref(),
        Some("second")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn transaction_restarts_with_an_overlay_that_another_client_writes() {
    let cfg = common::fresh_config();
    let db = common::create_db(&cfg).await;
    common::seed_path(&db, PATH_ID, &[], Strategy::BottomUp).await;
    store::cmd_lesson_upsert(&db, "fnd.1.1.1", "cached".into(), Some(PATH_ID))
        .await
        .unwrap();
    let other = Db::connect(&cfg).unwrap();
    let mut seen = Vec::new();

    db.transact(&Want::path(Some(PATH_ID)), |view, _| {
        seen.push(
            view.overlay().unwrap().atoms["fnd.1.1.1"]
                .lesson
                .clone()
                .unwrap(),
        );
        if seen.len() == 1 {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    store::cmd_lesson_upsert(&other, "fnd.1.1.1", "new".into(), None)
                        .await
                        .unwrap();
                });
            });
        }
        Ok(())
    })
    .await
    .unwrap();

    assert_eq!(seen.first().map(String::as_str), Some("cached"));
    assert_eq!(seen.last().map(String::as_str), Some("new"), "{seen:?}");
}

/// Transport that replaces the stored overlay version before each read and each transaction begin.
struct OverlayChurnTransport {
    inner: HttpTransport,
    raw: baserunna_client::Db,
    begins: Arc<AtomicUsize>,
}

impl Transport for OverlayChurnTransport {
    fn send(
        &self,
        req: http::Request<Bytes>,
    ) -> BoxFuture<'_, Result<http::Response<Bytes>, TransportError>> {
        Box::pin(async move {
            let path = req.uri().path();
            let begins = path.ends_with("/transactions");
            if begins || path.ends_with("/txn") {
                let version = format!("{:032x}", rand::random::<u128>());
                self.raw
                    .write(&[Op::put("overlay_version", version)])
                    .await
                    .unwrap();
            }
            if begins {
                self.begins.fetch_add(1, Ordering::Relaxed);
            }
            self.inner.send(req).await
        })
    }
}

#[tokio::test]
async fn overlay_changes_restart_a_transaction_within_the_retry_limit() {
    let cfg = common::fresh_config();
    common::seed_path(
        &common::create_db(&cfg).await,
        PATH_ID,
        &[],
        Strategy::BottomUp,
    )
    .await;
    let begins = Arc::<AtomicUsize>::default();
    let db = Db::with_transport(
        &cfg,
        OverlayChurnTransport {
            inner: cfg.transport().unwrap(),
            raw: raw_db(&cfg),
            begins: begins.clone(),
        },
    );

    let want = Want::path(Some(PATH_ID));
    let attempt = db.transact(&want, |_, _| Ok(()));
    let e = tokio::time::timeout(std::time::Duration::from_mins(1), attempt)
        .await
        .expect("the retry loop stops")
        .unwrap_err();

    assert!(e.to_string().contains("overlay_version"), "{e}");
    let begins = begins.load(Ordering::Relaxed);
    let limit = baserunna_client::RetryPolicy::default().txn_attempts;
    assert_eq!(begins, usize::try_from(limit).unwrap(), "{begins} attempts");
}

#[tokio::test]
async fn quiz_ids_stay_unique_under_concurrent_authoring() {
    let cfg = common::fresh_config();
    let db = common::create_db(&cfg).await;
    common::seed_path(&db, PATH_ID, &[], Strategy::BottomUp).await;
    store::cmd_lesson_upsert(&db, "fnd.1.1.1", "body".into(), Some(PATH_ID))
        .await
        .unwrap();
    let tasks: Vec<_> = (0..6)
        .map(|i| {
            let db = Db::connect(&cfg).unwrap();
            tokio::spawn(async move {
                store::cmd_quiz_create(
                    &db,
                    "fnd.1.1.1",
                    QuizDraft {
                        difficulty: Difficulty::Easy,
                        question: format!("q{i}"),
                        answer: "a".into(),
                        rubric: None,
                        kind: QuizType::FreeText,
                    },
                    Some(PATH_ID),
                )
                .await
                .unwrap()
            })
        })
        .collect();
    let mut ids = Vec::new();
    for t in tasks {
        ids.push(t.await.unwrap());
    }
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 6, "ids: {ids:?}");
    assert_eq!(
        common::overlay(&db).await.atoms["fnd.1.1.1"].quizzes.len(),
        6
    );
}

#[tokio::test]
async fn own_overlay_writes_keep_the_cache_valid() {
    let cfg = common::fresh_config();
    common::create_db(&cfg).await;
    let (db, traffic) = counting_db(&cfg);
    common::seed_path(&db, PATH_ID, &[], Strategy::BottomUp).await;
    let body = "y".repeat(20_000);
    store::cmd_lesson_upsert(&db, "fnd.1.1.1", body.clone(), Some(PATH_ID))
        .await
        .unwrap();
    let quiz = store::cmd_quiz_create(
        &db,
        "fnd.1.1.1",
        QuizDraft {
            difficulty: Difficulty::Easy,
            question: "q".into(),
            answer: "a".into(),
            rubric: None,
            kind: QuizType::FreeText,
        },
        Some(PATH_ID),
    )
    .await
    .unwrap();
    store::cmd_quiz_delete(&db, &quiz, Some(PATH_ID))
        .await
        .unwrap();
    traffic.received.store(0, Ordering::Relaxed);

    let view = db
        .snapshot(&mathtutor::view::Want::path(Some(PATH_ID)))
        .await
        .unwrap();
    let warm = traffic.received.load(Ordering::Relaxed);
    assert!(
        warm < 2_000,
        "the writer's cache stays valid ({warm} bytes)"
    );
    let cached = view.overlay().unwrap().atoms["fnd.1.1.1"].clone();
    let fresh = common::overlay(&db).await;
    let stored = common::overlay(&Db::connect(&cfg).unwrap()).await;
    assert_eq!(cached.lesson.as_deref(), Some(body.as_str()));
    assert_eq!(
        format!("{cached:?}"),
        format!("{:?}", stored.atoms["fnd.1.1.1"])
    );
    assert_eq!(format!("{:?}", fresh.atoms), format!("{:?}", stored.atoms));
}

#[tokio::test]
async fn path_next_makes_its_path_the_most_recent_even_without_an_event() {
    let db = common::empty_db().await;
    let graph = mathtutor::graph::Curriculum::embedded().graph().unwrap();
    let mut lessonless: Vec<&String> = graph
        .by_id
        .iter()
        .filter(|(_, c)| {
            c.children_ids.is_empty() && c.lesson.is_none() && c.prerequisites.is_empty()
        })
        .map(|(id, _)| id)
        .collect();
    lessonless.sort();
    let target = lessonless[0].as_str();
    common::seed_path(&db, "p_a_older", &[target], Strategy::BottomUp).await;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    common::seed_path(&db, "p_z_newer", &[target], Strategy::BottomUp).await;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    let envelope = mathtutor::scheduler::compute_next(
        &db,
        Some("p_a_older"),
        mathtutor::scheduler::NextMode::Default,
        mathtutor::surface::Surface::Cli,
    )
    .await
    .unwrap();

    assert_eq!(
        serde_json::to_value(&envelope).unwrap()["action"],
        "create_lesson"
    );
    let view = db
        .snapshot(&mathtutor::view::Want::path(None))
        .await
        .unwrap();
    assert_eq!(view.path().unwrap().file.id, "p_a_older");
}

#[tokio::test]
async fn write_for_a_path_makes_it_the_most_recent() {
    let db = common::empty_db().await;
    common::seed_path(&db, "p_a_older", &[], Strategy::BottomUp).await;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    common::seed_path(&db, "p_z_newer", &[], Strategy::BottomUp).await;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    mathtutor::path::cmd_path_strategy(&db, Some("p_a_older"), Strategy::TopDown)
        .await
        .unwrap();

    let view = db
        .snapshot(&mathtutor::view::Want::path(None))
        .await
        .unwrap();
    assert_eq!(view.path().unwrap().file.id, "p_a_older");
}

#[tokio::test]
async fn a_view_without_the_overlay_has_no_merged_graph() {
    let db = common::fresh_db(PATH_ID).await;

    let view = db
        .snapshot(&Want::path(Some(PATH_ID)).without_overlay())
        .await
        .unwrap();

    assert!(matches!(view.graph(), Err(Error::OverlayNotLoaded)));
    assert!(matches!(view.overlay(), Err(Error::OverlayNotLoaded)));
    assert!(view.path().is_ok());
}

#[tokio::test]
async fn merged_graph_is_shared_until_the_overlay_version_changes() {
    let db = common::fresh_db(PATH_ID).await;
    let merged = async || {
        db.snapshot(&Want::overlay())
            .await
            .unwrap()
            .graph()
            .unwrap()
    };
    store::cmd_lesson_upsert(&db, "fnd.1.1.1", "first".into(), Some(PATH_ID))
        .await
        .unwrap();
    let first = merged().await;
    assert!(Arc::ptr_eq(&first, &merged().await));

    store::cmd_lesson_upsert(&db, "fnd.1.1.1", "second".into(), Some(PATH_ID))
        .await
        .unwrap();
    let second = merged().await;
    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(
        second.atom("fnd.1.1.1").unwrap().lesson.as_deref(),
        Some("second")
    );
}

#[tokio::test]
async fn commands_use_the_curriculum_of_the_database_config() {
    let dir = common::write_graph(
        "
  - id: ta.1
    name: cluster
    children:
      - id: ta.1.1
        name: local atom
        description: d
        terminal: true
",
    );
    let cfg = DbConfig {
        curriculum: Curriculum::open(Some(dir.path().into())),
        ..common::fresh_config()
    };
    let db = common::create_db(&cfg).await;
    let id = path::cmd_path_new(&db, "local", &["ta.1.1".into()], Strategy::BottomUp)
        .await
        .unwrap();
    store::cmd_lesson_upsert(&db, "ta.1.1", "body".into(), Some(&id))
        .await
        .unwrap();
    let state = mathtutor::state::compute_state(&db, Some(&id))
        .await
        .unwrap();
    assert_eq!(state.next.map(|a| a.name).as_deref(), Some("local atom"));
}

#[tokio::test]
async fn graph_lookup_rejects_an_unknown_path() {
    let db = common::empty_db().await;
    let lookup = mathtutor::discover::Lookup::Db {
        db: &db,
        path: Some("typo"),
    };
    let result = mathtutor::discover::graph_for(lookup).await;
    assert!(matches!(result, Err(Error::NoPath)));
}

struct DisconnectedTransport;

impl Transport for DisconnectedTransport {
    fn send(
        &self,
        _req: http::Request<Bytes>,
    ) -> BoxFuture<'_, Result<http::Response<Bytes>, TransportError>> {
        Box::pin(async { Err(TransportError::Unknown("connection reset".into())) })
    }
}

#[tokio::test]
async fn failed_read_reports_unapplied_outcome() {
    let db = Db::with_transport(&common::fresh_config(), DisconnectedTransport);

    let e = db
        .snapshot(&mathtutor::view::Want::overlay())
        .await
        .unwrap_err();

    assert!(!e.outcome_is_unknown(), "{e}");
    assert_eq!(e.exit_code(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn retried_command_keeps_the_most_recent_path_of_its_first_attempt() {
    let db = common::fresh_db("p_old").await;
    common::seed_path(&db, "p_new", &[], Strategy::BottomUp).await;
    let answered = |path: &str| Event {
        path: path.into(),
        ..common::answered("atom.x.q1", Rating::Good)
    };
    let want_old = mathtutor::view::Want::path(Some("p_old"));
    db.transact(&want_old, |view, changes| {
        changes.append(view, &answered("p_old"))
    })
    .await
    .unwrap();
    let other = db.clone();
    let mut selected = Vec::new();

    db.transact(&mathtutor::view::Want::path(None), |view, changes| {
        selected.push(view.path()?.file.id.clone());
        changes.append(view, &answered(&view.path()?.file.id))?;
        if selected.len() == 1 {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    other
                        .transact(
                            &mathtutor::view::Want::path(Some("p_new")),
                            |view, changes| changes.append(view, &answered("p_new")),
                        )
                        .await
                        .unwrap();
                });
            });
        }
        Ok(())
    })
    .await
    .unwrap();

    assert!(selected.iter().all(|id| id == "p_old"), "{selected:?}");
}

#[tokio::test]
async fn recreating_database_invalidates_overlay_cache() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = DbConfig {
        cache_dir: Some(dir.path().to_path_buf()),
        ..common::fresh_config()
    };
    let upsert_lesson = async |cfg: &DbConfig, body: &str| {
        let db = common::create_db(cfg).await;
        common::seed_path(&db, PATH_ID, &[], Strategy::BottomUp).await;
        store::cmd_lesson_upsert(&db, "fnd.1.1.1", body.into(), Some(PATH_ID))
            .await
            .unwrap();
        db
    };
    upsert_lesson(&cfg, "old").await;
    raw_db(&cfg).delete().await.unwrap();

    let uncached = DbConfig {
        cache_dir: None,
        ..cfg.clone()
    };
    upsert_lesson(&uncached, "new").await;
    let reader = Db::connect(&cfg).unwrap();
    let view = reader
        .snapshot(&mathtutor::view::Want::overlay())
        .await
        .unwrap();

    assert_eq!(
        view.overlay().unwrap().atoms["fnd.1.1.1"].lesson.as_deref(),
        Some("new")
    );
}

#[tokio::test]
async fn a_handle_reads_the_cache_file_only_when_a_command_needs_the_overlay() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = DbConfig {
        cache_dir: Some(dir.path().to_path_buf()),
        ..common::fresh_config()
    };
    let first = common::create_db(&cfg).await;
    common::seed_path(&first, PATH_ID, &[], Strategy::BottomUp).await;
    let body = "w".repeat(20_000);
    store::cmd_lesson_upsert(&first, "fnd.1.1.1", body.clone(), Some(PATH_ID))
        .await
        .unwrap();
    let (second, traffic) = counting_db(&cfg);
    second
        .snapshot(&Want::path(Some(PATH_ID)).without_overlay())
        .await
        .unwrap();

    for file in std::fs::read_dir(dir.path()).unwrap() {
        std::fs::remove_file(file.unwrap().path()).unwrap();
    }
    traffic.received.store(0, Ordering::Relaxed);
    let view = second.snapshot(&Want::overlay()).await.unwrap();

    assert_eq!(
        view.overlay().unwrap().atoms["fnd.1.1.1"].lesson.as_deref(),
        Some(body.as_str())
    );
    let cold = traffic.received.load(Ordering::Relaxed);
    assert!(
        cold > 20_000,
        "the overlay came from the server ({cold} bytes)"
    );
}

#[tokio::test]
async fn overlay_cache_on_disk_serves_the_next_process_until_the_overlay_changes() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = DbConfig {
        cache_dir: Some(dir.path().to_path_buf()),
        ..common::fresh_config()
    };
    let first = common::create_db(&cfg).await;
    common::seed_path(&first, PATH_ID, &[], Strategy::BottomUp).await;
    let body = "z".repeat(20_000);
    store::cmd_lesson_upsert(&first, "fnd.1.1.1", body.clone(), Some(PATH_ID))
        .await
        .unwrap();
    drop(first);

    let (second, traffic) = counting_db(&cfg);
    let view = second
        .snapshot(&mathtutor::view::Want::path(Some(PATH_ID)))
        .await
        .unwrap();
    assert_eq!(
        view.overlay().unwrap().atoms["fnd.1.1.1"].lesson.as_deref(),
        Some(body.as_str())
    );
    let warm = traffic.received.swap(0, Ordering::Relaxed);
    assert!(warm < 2_000, "the disk copy was used ({warm} bytes)");

    let other = Db::connect(&DbConfig {
        cache_dir: None,
        ..cfg.clone()
    })
    .unwrap();
    store::cmd_lesson_upsert(&other, "fnd.1.1.1", "new".into(), Some(PATH_ID))
        .await
        .unwrap();
    let third = Db::connect(&cfg).unwrap();
    let view = third
        .snapshot(&mathtutor::view::Want::path(Some(PATH_ID)))
        .await
        .unwrap();
    assert_eq!(
        view.overlay().unwrap().atoms["fnd.1.1.1"].lesson.as_deref(),
        Some("new")
    );
}

#[tokio::test]
async fn a_rejected_token_names_the_server_and_the_settings() {
    let cfg = common::fresh_config();
    common::create_db(&cfg).await;
    let wrong = DbConfig {
        token: "not-a-key".into(),
        ..cfg.clone()
    };
    let db = Db::connect(&wrong).unwrap();
    let errors = [
        db.require_database().await.unwrap_err(),
        db.create_if_missing().await.unwrap_err(),
        db.snapshot(&Want::overlay()).await.unwrap_err(),
        path::cmd_path_new(&db, "g", &["fnd.1.1.1".into()], Strategy::BottomUp)
            .await
            .unwrap_err(),
    ];
    for e in errors {
        let message = e.to_string();
        assert!(matches!(e, Error::TokenRejected { .. }), "{e:?}");
        assert!(message.contains(&cfg.url), "{message}");
        assert!(message.contains("BASERUNNA_URL"), "{message}");
        assert!(message.contains("BASERUNNA_TOKEN"), "{message}");
        assert_eq!(e.exit_code(), 2);
    }
}
