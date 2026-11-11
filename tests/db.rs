//! Integration tests for database initialization, streams, concurrency, and caching.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use baserunna_client::api::ListTablesQuery;
use baserunna_client::bytes::Bytes;
use baserunna_client::{BoxFuture, Client, HttpTransport, Transport, TransportError, http};
use chrono::{Duration, Utc};
use mathtutor::db::{Db, DbConfig};
use mathtutor::event_log::{self, Event, EventKind, EventPayload};
use mathtutor::types::{Difficulty, QuizType, Rating, Strategy};
use mathtutor::{Error, answer, cards, overlay, store};

mod common;

use common::PATH_ID;

const ATOM: &str = "fnd.2.1.1";

const TABLES: [&str; 10] = [
    "cards",
    "lessons",
    "oauth_access_tokens",
    "oauth_clients",
    "oauth_codes",
    "oauth_refresh_tokens",
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
    Db::open(&cfg).await.expect("open after creation");
}

#[tokio::test]
async fn open_rejects_a_missing_database_without_creating_it() {
    let cfg = common::fresh_config();

    let e = Db::open(&cfg).await.unwrap_err();

    let message = e.to_string();
    assert!(matches!(e, Error::DatabaseNotFound { .. }), "{e:?}");
    assert!(message.contains(&format!("'{}'", cfg.db)), "{message}");
    assert!(message.contains(&cfg.url), "{message}");
    assert!(message.contains("BASERUNNA_DB"), "{message}");
    assert_eq!(e.exit_code(), 2);
    let client = Client::new(cfg.transport().unwrap()).with_token(&cfg.token);
    let info = client.db(&cfg.db).info().await;
    assert!(info.is_err(), "open must not create the database: {info:?}");
}

#[tokio::test]
async fn open_rejects_a_database_without_math_tutor_tables() {
    let cfg = common::fresh_config();
    let client = Client::new(cfg.transport().unwrap()).with_token(&cfg.token);
    client.db(&cfg.db).ensure().await.unwrap();

    let e = Db::open(&cfg).await.unwrap_err();

    assert!(matches!(e, Error::DatabaseNotFound { .. }), "{e:?}");
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
            let p = view.path().unwrap().file.clone();
            (p.id, p.updated_at)
        }
    };
    assert_eq!(recent(&db).await.0, "p_b", "the last created path");
    store::cmd_lesson_upsert(&db, "fnd.1.1.1", "body".into(), Some("p_a"), None)
        .await
        .unwrap();
    let (id, at) = recent(&db).await;
    assert_eq!(id, "p_a");
    assert!(at > Utc::now() - Duration::minutes(1), "activity time {at}");
    let fresh = Db::connect(&cfg).unwrap();
    assert_eq!(recent(&fresh).await.0, "p_a");
    store::cmd_lesson_upsert(&db, "fnd.1.1.1", "again".into(), Some("p_b"), None)
        .await
        .unwrap();
    assert_eq!(
        recent(&db).await.0,
        "p_b",
        "the selection follows the newer activity"
    );
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

    let db = Db::open(&cfg).await.expect("open");
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
    let t0 = Utc::now() - Duration::hours(1);
    let full = Event {
        ts: t0,
        kind: EventKind::QuizAnswered,
        path: PATH_ID.into(),
        atom: Some("a".into()),
        quiz: Some("a.q1".into()),
        payload: EventPayload {
            rating: Some(Rating::Hard),
            reason: Some("why \"quoted\" ✓".into()),
            user_answer: Some("multi\nline".into()),
        },
    };
    let bare = event_log::lesson_taught(PATH_ID.into(), "b".into());
    common::append(&db, &[full.clone(), bare.clone()]).await;
    common::append(
        &db,
        &[event_log::quiz_removed(
            PATH_ID.into(),
            "a".into(),
            "a.q1".into(),
        )],
    )
    .await;

    let events = event_log::load(&db, PATH_ID).await.unwrap();
    let kinds: Vec<EventKind> = events.iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        [
            EventKind::PathCreated,
            EventKind::QuizAnswered,
            EventKind::LessonTaught,
            EventKind::QuizRemoved
        ]
    );
    let got = &events[1];
    assert_eq!(got.ts.timestamp_millis(), t0.timestamp_millis());
    assert_eq!(got.path, PATH_ID);
    assert_eq!(got.atom.as_deref(), Some("a"));
    assert_eq!(got.quiz.as_deref(), Some("a.q1"));
    assert_eq!(got.payload.rating, Some(Rating::Hard));
    assert_eq!(got.payload.reason.as_deref(), Some("why \"quoted\" ✓"));
    assert_eq!(got.payload.user_answer.as_deref(), Some("multi\nline"));
    let b = &events[2];
    assert_eq!(b.atom.as_deref(), Some("b"));
    assert!(b.quiz.is_none());
    assert!(b.payload.is_empty());
}

#[tokio::test]
async fn event_log_pages_across_multiple_reads() {
    let db = common::fresh_db(PATH_ID).await;
    let ts = Utc::now();
    let events: Vec<Event> = (0..1205)
        .map(|i| common::presented_at(&format!("q{i}"), ts))
        .collect();
    for chunk in events.chunks(500) {
        common::append(&db, chunk).await;
    }
    let loaded = event_log::load(&db, PATH_ID).await.unwrap();
    assert_eq!(loaded.len(), 1206);
    assert_eq!(loaded[1].quiz.as_deref(), Some("q0"));
    assert_eq!(loaded[1205].quiz.as_deref(), Some("q1204"));
}

#[tokio::test]
async fn concurrent_answers_are_recorded_once_each() {
    let db = common::fresh_db(PATH_ID).await;
    let quiz = store::cmd_quiz_create(
        &db,
        ATOM,
        Difficulty::Easy,
        "q?".into(),
        "a".into(),
        None,
        QuizType::FreeText,
        Some(PATH_ID),
        None,
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
                answer::cmd_quiz_answer(&db, &quiz, rating, None, Some(PATH_ID), None).await
            })
        })
        .collect();
    for t in tasks {
        t.await.unwrap().expect("answer");
    }

    let ps = common::path_state(&db, PATH_ID).await;
    let card = &ps.cards[&quiz];
    assert_eq!(card.reps, 8);
    assert_eq!(card.lapses, 4);
    assert_eq!(card.recent.len(), 8);
    let answered = event_log::load(&db, PATH_ID)
        .await
        .unwrap()
        .iter()
        .filter(|e| e.kind == EventKind::QuizAnswered)
        .count();
    assert_eq!(answered, 8);
}

#[tokio::test]
async fn recompute_reproduces_incremental_rows_exactly() {
    let db = common::fresh_db(PATH_ID).await;
    let t0 = Utc::now() - Duration::days(30);
    common::append(
        &db,
        &[
            common::taught_at("a", t0),
            common::lesson_authored("b"),
            common::presented_at("a.q1", t0),
            common::answered_at("a.q1", Rating::Again, t0),
        ],
    )
    .await;
    common::append(
        &db,
        &[
            common::taught_at("a", t0 + Duration::days(1)),
            common::presented_at("a.q1", t0 + Duration::days(2)),
            common::answered_at("a.q1", Rating::Good, t0 + Duration::days(2)),
            common::answered_at("a.q2", Rating::Easy, t0 + Duration::days(3)),
        ],
    )
    .await;

    let before = common::path_state(&db, PATH_ID).await;
    cards::recompute(&db, PATH_ID).await.unwrap();
    let after = common::path_state(&db, PATH_ID).await;

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

/// Transport that counts response bytes.
struct CountingTransport {
    inner: HttpTransport,
    received: Arc<AtomicUsize>,
}

impl Transport for CountingTransport {
    fn send(
        &self,
        req: http::Request<Bytes>,
    ) -> BoxFuture<'_, Result<http::Response<Bytes>, TransportError>> {
        Box::pin(async move {
            let resp = self.inner.send(req).await?;
            self.received
                .fetch_add(resp.body().len(), Ordering::Relaxed);
            Ok(resp)
        })
    }
}

fn counting_db(cfg: &DbConfig) -> (Db, Arc<AtomicUsize>) {
    let received = Arc::new(AtomicUsize::new(0));
    let inner = cfg.transport().unwrap();
    let t = CountingTransport {
        inner,
        received: received.clone(),
    };
    (Db::with_transport(cfg, t), received)
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

    let (reader, received) = counting_db(&cfg);
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
        first.overlay.atoms["fnd.1.1.1"].lesson.as_deref(),
        Some(body.as_str())
    );
    let cold = received.swap(0, Ordering::Relaxed);
    assert!(
        cold > 20_000,
        "first read downloads the overlay ({cold} bytes)"
    );

    let second = read(&reader).await;
    assert_eq!(
        second.overlay.atoms["fnd.1.1.1"].lesson.as_deref(),
        Some(body.as_str())
    );
    let warm = received.swap(0, Ordering::Relaxed);
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
        third.overlay.atoms["fnd.1.1.1"].lesson.as_deref(),
        Some("revised")
    );
}

#[tokio::test]
async fn quiz_ids_stay_unique_under_concurrent_authoring() {
    let cfg = common::fresh_config();
    let db = common::create_db(&cfg).await;
    common::seed_path(&db, PATH_ID, &[], Strategy::BottomUp).await;
    store::cmd_lesson_upsert(&db, "fnd.1.1.1", "body".into(), Some(PATH_ID), None)
        .await
        .unwrap();
    let tasks: Vec<_> = (0..6)
        .map(|i| {
            let db = Db::connect(&cfg).unwrap();
            tokio::spawn(async move {
                store::cmd_quiz_create(
                    &db,
                    "fnd.1.1.1",
                    Difficulty::Easy,
                    format!("q{i}"),
                    "a".into(),
                    None,
                    QuizType::FreeText,
                    Some(PATH_ID),
                    None,
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
    let (db, received) = counting_db(&cfg);
    common::seed_path(&db, PATH_ID, &[], Strategy::BottomUp).await;
    let body = "y".repeat(20_000);
    store::cmd_lesson_upsert(&db, "fnd.1.1.1", body.clone(), Some(PATH_ID), None)
        .await
        .unwrap();
    let quiz = store::cmd_quiz_create(
        &db,
        "fnd.1.1.1",
        Difficulty::Easy,
        "q".into(),
        "a".into(),
        None,
        QuizType::FreeText,
        Some(PATH_ID),
        None,
    )
    .await
    .unwrap();
    store::cmd_quiz_delete(&db, &quiz, Some(PATH_ID), None)
        .await
        .unwrap();
    received.store(0, Ordering::Relaxed);

    let view = db
        .snapshot(&mathtutor::view::Want::path(Some(PATH_ID)))
        .await
        .unwrap();
    let warm = received.load(Ordering::Relaxed);
    assert!(
        warm < 2_000,
        "the writer's cache stays valid ({warm} bytes)"
    );
    let cached = view.overlay.atoms["fnd.1.1.1"].clone();
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
    let graph = mathtutor::graph::Graph::load_default(None).unwrap();
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
        None,
        mathtutor::scheduler::NextMode::Default,
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
async fn graph_lookup_rejects_an_unknown_path() {
    let db = common::empty_db().await;
    let result = mathtutor::discover::graph_for(Some(&db), Some("typo"), None).await;
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
        ts: Utc::now(),
        kind: EventKind::QuizAnswered,
        path: path.into(),
        atom: Some("atom.x".into()),
        quiz: Some("atom.x.q1".into()),
        payload: EventPayload {
            rating: Some(Rating::Good),
            reason: None,
            user_answer: None,
        },
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
        store::cmd_lesson_upsert(&db, "fnd.1.1.1", body.into(), Some(PATH_ID), None)
            .await
            .unwrap();
        db
    };
    let first = upsert_lesson(&cfg, "old").await;
    first.client().db(&cfg.db).delete().await.unwrap();

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
        view.overlay.atoms["fnd.1.1.1"].lesson.as_deref(),
        Some("new")
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
    store::cmd_lesson_upsert(&first, "fnd.1.1.1", body.clone(), Some(PATH_ID), None)
        .await
        .unwrap();
    drop(first);

    let (second, received) = counting_db(&cfg);
    let view = second
        .snapshot(&mathtutor::view::Want::path(Some(PATH_ID)))
        .await
        .unwrap();
    assert_eq!(
        view.overlay.atoms["fnd.1.1.1"].lesson.as_deref(),
        Some(body.as_str())
    );
    let warm = received.swap(0, Ordering::Relaxed);
    assert!(warm < 2_000, "the disk copy was used ({warm} bytes)");

    let other = Db::connect(&DbConfig {
        cache_dir: None,
        ..cfg.clone()
    })
    .unwrap();
    store::cmd_lesson_upsert(&other, "fnd.1.1.1", "new".into(), Some(PATH_ID), None)
        .await
        .unwrap();
    let third = Db::connect(&cfg).unwrap();
    let view = third
        .snapshot(&mathtutor::view::Want::path(Some(PATH_ID)))
        .await
        .unwrap();
    assert_eq!(
        view.overlay.atoms["fnd.1.1.1"].lesson.as_deref(),
        Some("new")
    );
}
