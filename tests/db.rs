//! Integration tests for the Baserunna storage layer: database and
//! schema creation, the per-path event stream, conflict retries under
//! concurrent transactions, and rebuilding derived rows from the log.

use chrono::{Duration, Utc};
use mathtutor::db::{Db, DbConfig};
use mathtutor::event_log::{self, Event, EventKind, EventPayload};
use mathtutor::types::{Difficulty, QuizType, Rating, Strategy};
use mathtutor::{answer, cards, overlay, store};

mod common;

use common::PATH_ID;

/// An atom with a shipped lesson, so quizzes can be authored on it.
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
    let r: serde_json::Value = reqwest::Client::new()
        .get(format!("{}/v1/dbs/{}/tables", cfg.url, cfg.db))
        .bearer_auth(&cfg.token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut names: Vec<String> = r["tables"]
        .as_array()
        .expect("tables")
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn open_creates_database_and_every_table() {
    let cfg = common::fresh_config();
    Db::open(&cfg).await.expect("open");
    assert_eq!(table_names(&cfg).await, TABLES);
}

#[tokio::test]
async fn most_recent_path_follows_logged_events() {
    let cfg = common::fresh_config();
    let db = Db::open(&cfg).await.unwrap();
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
    // A fresh handle has no hint and must find the same path.
    let fresh = Db::connect(&cfg).unwrap();
    assert_eq!(recent(&fresh).await.0, "p_a");
    store::cmd_lesson_upsert(&db, "fnd.1.1.1", "again".into(), Some("p_b"), None)
        .await
        .unwrap();
    assert_eq!(
        recent(&db).await.0,
        "p_b",
        "the hint follows the newer activity"
    );
}

#[tokio::test]
async fn reopening_is_a_no_op_that_preserves_data() {
    let cfg = common::fresh_config();
    let db = Db::open(&cfg).await.expect("first open");
    common::seed_path(&db, PATH_ID, &["fnd.2"], Strategy::TopDown).await;
    common::write(&db, PATH_ID, |_, changes| {
        overlay::upsert_lesson(changes, ATOM, "kept");
    })
    .await;

    let db = Db::open(&cfg).await.expect("second open");
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
    // Timestamps are stored at millisecond precision.
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
async fn event_log_pages_past_one_read() {
    // A read returns at most 1000 records; `load` must follow `next_seq`.
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
async fn concurrent_answers_all_land_exactly_once() {
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

/// Bytes received by every request a handle makes.
fn counting(db: Db) -> (Db, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let received = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let r = received.clone();
    let db = db.observed(std::sync::Arc::new(move |s: mathtutor::db::Sample| {
        r.fetch_add(s.received_bytes, std::sync::atomic::Ordering::Relaxed);
    }));
    (db, received)
}

#[tokio::test]
async fn cached_overlay_is_reused_until_another_client_writes() {
    let cfg = common::fresh_config();
    let writer = Db::open(&cfg).await.unwrap();
    common::seed_path(&writer, PATH_ID, &[], Strategy::BottomUp).await;
    let body = "x".repeat(20_000);
    common::write(&writer, PATH_ID, |_, c| {
        overlay::upsert_lesson(c, "fnd.1.1.1", &body);
    })
    .await;

    let (reader, received) = counting(Db::connect(&cfg).unwrap());
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
    let cold = received.swap(0, std::sync::atomic::Ordering::Relaxed);
    assert!(
        cold > 20_000,
        "first read downloads the overlay ({cold} bytes)"
    );

    let second = read(&reader).await;
    assert_eq!(
        second.overlay.atoms["fnd.1.1.1"].lesson.as_deref(),
        Some(body.as_str())
    );
    let warm = received.swap(0, std::sync::atomic::Ordering::Relaxed);
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
async fn quiz_ids_stay_unique_when_clients_author_concurrently() {
    let cfg = common::fresh_config();
    let db = Db::open(&cfg).await.unwrap();
    common::seed_path(&db, PATH_ID, &[], Strategy::BottomUp).await;
    store::cmd_lesson_upsert(&db, "fnd.1.1.1", "body".into(), Some(PATH_ID), None)
        .await
        .unwrap();
    // Separate handles keep separate overlay caches.
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
    let (db, received) = counting(Db::open(&cfg).await.unwrap());
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
    received.store(0, std::sync::atomic::Ordering::Relaxed);

    let view = db
        .snapshot(&mathtutor::view::Want::path(Some(PATH_ID)))
        .await
        .unwrap();
    let warm = received.load(std::sync::atomic::Ordering::Relaxed);
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
async fn overlay_cache_on_disk_serves_the_next_process_until_the_overlay_changes() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = DbConfig {
        cache_dir: Some(dir.path().to_path_buf()),
        ..common::fresh_config()
    };
    let first = Db::open(&cfg).await.unwrap();
    common::seed_path(&first, PATH_ID, &[], Strategy::BottomUp).await;
    let body = "z".repeat(20_000);
    store::cmd_lesson_upsert(&first, "fnd.1.1.1", body.clone(), Some(PATH_ID), None)
        .await
        .unwrap();
    drop(first);

    // A later process starts from the copy on disk.
    let (second, received) = counting(Db::connect(&cfg).unwrap());
    let view = second
        .snapshot(&mathtutor::view::Want::path(Some(PATH_ID)))
        .await
        .unwrap();
    assert_eq!(
        view.overlay.atoms["fnd.1.1.1"].lesson.as_deref(),
        Some(body.as_str())
    );
    let warm = received.swap(0, std::sync::atomic::Ordering::Relaxed);
    assert!(warm < 2_000, "the disk copy was used ({warm} bytes)");

    // A write by a client without the disk copy makes it stale.
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
