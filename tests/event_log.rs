//! Integration tests for the SQL-backed event log and its FSRS
//! write-through cache.
//!
//! Each test opens a fresh local libSQL database under a `tempdir`,
//! seeds it with one path row, then exercises `event_log::append` /
//! `event_log::load` and the `cards::*` helpers directly. The aim is
//! to pin three contracts:
//!
//! 1. Events round-trip lossless-ly through SQL (including payload).
//! 2. `QuizAnswered` updates the `cards` cache as a write-through
//!    side effect of `append`.
//! 3. `cards::recompute` rebuilds the cache deterministically from
//!    the event log alone, matching live append behavior.

use chrono::{Duration, Utc};
use libsql::params;
use mathtutor::cards;
use mathtutor::event_log::{self, Event, EventKind, EventPayload};
use mathtutor::types::Rating;
use tempfile::TempDir;

mod common;

const PATH_ID: &str = "p_test";

#[tokio::test]
async fn append_and_load_round_trip() {
    let tmp = TempDir::new().unwrap();
    let conn = common::fresh_db(&tmp, PATH_ID).await;

    event_log::append(
        &conn,
        &event_log::lesson_authored(PATH_ID.into(), "atom.x".into()),
    )
    .await
    .expect("append lesson_authored");

    event_log::append(
        &conn,
        &event_log::quiz_presented(PATH_ID.into(), "atom.x".into(), "atom.x.q1".into()),
    )
    .await
    .expect("append quiz_presented");

    let events = event_log::load(&conn, PATH_ID).await.expect("load");
    assert_eq!(events.len(), 2);
    assert!(matches!(events[0].kind, EventKind::LessonAuthored));
    assert_eq!(events[0].atom.as_deref(), Some("atom.x"));
    assert!(matches!(events[1].kind, EventKind::QuizPresented));
    assert_eq!(events[1].quiz.as_deref(), Some("atom.x.q1"));
}

#[tokio::test]
async fn load_returns_events_in_insertion_order() {
    // Two events with the same `ts` must still come back in insert
    // order — the SQL `ORDER BY id ASC` is what guarantees replayability
    // for FSRS even when wall-clock ticks coincide.
    let tmp = TempDir::new().unwrap();
    let conn = common::fresh_db(&tmp, PATH_ID).await;

    let ts = Utc::now();
    let mk = |quiz: &str| Event {
        ts,
        kind: EventKind::QuizPresented,
        path: PATH_ID.into(),
        atom: Some("atom.x".into()),
        quiz: Some(quiz.into()),
        payload: EventPayload::default(),
    };
    event_log::append(&conn, &mk("q1")).await.unwrap();
    event_log::append(&conn, &mk("q2")).await.unwrap();
    event_log::append(&conn, &mk("q3")).await.unwrap();

    let events = event_log::load(&conn, PATH_ID).await.unwrap();
    let quizzes: Vec<_> = events.iter().map(|e| e.quiz.clone().unwrap()).collect();
    assert_eq!(quizzes, vec!["q1".to_string(), "q2".into(), "q3".into()]);
}

#[tokio::test]
async fn append_preserves_payload_user_answer_and_rating() {
    let tmp = TempDir::new().unwrap();
    let conn = common::fresh_db(&tmp, PATH_ID).await;

    event_log::append(
        &conn,
        &event_log::quiz_answered(
            PATH_ID.into(),
            Some("atom.x".into()),
            "atom.x.q1".into(),
            Rating::Good,
            Some("user said cosine".into()),
        ),
    )
    .await
    .unwrap();

    let events = event_log::load(&conn, PATH_ID).await.unwrap();
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert!(matches!(e.kind, EventKind::QuizAnswered));
    assert_eq!(e.payload.rating, Some(Rating::Good));
    assert_eq!(e.payload.user_answer.as_deref(), Some("user said cosine"));
}

#[tokio::test]
async fn append_only_touches_the_named_path() {
    // `load` must be path-scoped: events for one path must not leak
    // into another path's view, even when their ids are sequential.
    let tmp = TempDir::new().unwrap();
    let conn = common::fresh_db(&tmp, PATH_ID).await;
    conn.execute(
        "INSERT INTO paths(id, goal, created_at) VALUES (?, ?, ?)",
        params!["p_other", "other", "2026-05-26T00:00:00Z"],
    )
    .await
    .unwrap();

    event_log::append(
        &conn,
        &event_log::lesson_authored(PATH_ID.into(), "atom.x".into()),
    )
    .await
    .unwrap();
    event_log::append(
        &conn,
        &event_log::lesson_authored("p_other".into(), "atom.y".into()),
    )
    .await
    .unwrap();

    let mine = event_log::load(&conn, PATH_ID).await.unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].atom.as_deref(), Some("atom.x"));

    let other = event_log::load(&conn, "p_other").await.unwrap();
    assert_eq!(other.len(), 1);
    assert_eq!(other[0].atom.as_deref(), Some("atom.y"));
}

#[tokio::test]
async fn quiz_answered_creates_cards_row() {
    // The write-through invariant: a single `QuizAnswered` append
    // creates a `cards` row whose due date is in the future and whose
    // last_rating reflects the new answer.
    let tmp = TempDir::new().unwrap();
    let conn = common::fresh_db(&tmp, PATH_ID).await;

    let before = Utc::now();
    event_log::append(
        &conn,
        &event_log::quiz_answered(
            PATH_ID.into(),
            Some("atom.x".into()),
            "atom.x.q1".into(),
            Rating::Good,
            None,
        ),
    )
    .await
    .unwrap();

    let card = cards::read_card(&conn, PATH_ID, "atom.x.q1")
        .await
        .expect("read_card")
        .expect("cache row exists");
    assert_eq!(card.reps, 1);
    assert_eq!(card.lapses, 0);
    assert!(card.state.due > before, "due should be in the future");
    assert!(card.state.stability > 0.0);
    assert!(card.state.difficulty > 0.0);
}

#[tokio::test]
async fn again_rating_increments_lapses() {
    // `lapses` counts only `Again` ratings; a `Hard`/`Good`/`Easy`
    // review must leave it untouched.
    let tmp = TempDir::new().unwrap();
    let conn = common::fresh_db(&tmp, PATH_ID).await;

    for rating in [Rating::Again, Rating::Good, Rating::Again] {
        event_log::append(
            &conn,
            &event_log::quiz_answered(
                PATH_ID.into(),
                Some("atom.x".into()),
                "atom.x.q1".into(),
                rating,
                None,
            ),
        )
        .await
        .unwrap();
    }

    let card = cards::read_card(&conn, PATH_ID, "atom.x.q1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(card.reps, 3);
    assert_eq!(card.lapses, 2);
}

#[tokio::test]
async fn non_answer_events_do_not_create_cards_rows() {
    // Only `QuizAnswered` is supposed to update the cache. The other
    // event kinds (presented, authored, …) must leave the cards table
    // alone — otherwise the scheduler would treat unseen quizzes as
    // due-with-default-stability.
    let tmp = TempDir::new().unwrap();
    let conn = common::fresh_db(&tmp, PATH_ID).await;

    event_log::append(
        &conn,
        &event_log::quiz_presented(PATH_ID.into(), "atom.x".into(), "atom.x.q1".into()),
    )
    .await
    .unwrap();
    event_log::append(
        &conn,
        &event_log::quiz_authored(PATH_ID.into(), "atom.x".into(), "atom.x.q1".into()),
    )
    .await
    .unwrap();
    event_log::append(
        &conn,
        &event_log::lesson_taught(PATH_ID.into(), "atom.x".into()),
    )
    .await
    .unwrap();

    let card = cards::read_card(&conn, PATH_ID, "atom.x.q1").await.unwrap();
    assert!(card.is_none(), "no answer events ⇒ no cache row");
}

#[tokio::test]
async fn due_quizzes_returns_only_past_due_in_order() {
    // We seed the cards table directly so we can control due_at
    // without waiting wall-clock time. The query must filter by
    // path_id, respect `due_at <= now`, and return oldest-due first.
    let tmp = TempDir::new().unwrap();
    let conn = common::fresh_db(&tmp, PATH_ID).await;
    let now = Utc::now();

    let rows: [(&str, i64); 3] = [
        ("q.past_old", -3600),  // 1h ago — should be first
        ("q.past_recent", -60), // 1m ago — should be second
        ("q.future", 600),      // 10m from now — must be excluded
    ];
    for (quiz_id, offset_secs) in rows {
        let due = now + Duration::seconds(offset_secs);
        conn.execute(
            "INSERT INTO cards(path_id, quiz_id, stability, difficulty, due_at, \
                               last_reviewed_at, reps, lapses) \
             VALUES (?, ?, 1.0, 5.0, ?, ?, 1, 0)",
            params![
                PATH_ID,
                quiz_id,
                mathtutor::db::format_ts(due),
                mathtutor::db::format_ts(due - Duration::seconds(1)),
            ],
        )
        .await
        .unwrap();
    }

    let due = cards::due_quizzes(&conn, PATH_ID, now).await.unwrap();
    let ids: Vec<&str> = due.iter().map(|(q, _)| q.as_str()).collect();
    assert_eq!(ids, vec!["q.past_old", "q.past_recent"]);
}

#[tokio::test]
async fn recompute_rebuilds_cache_identically_to_live_append() {
    // Append a series of answers, snapshot the resulting cache row,
    // wipe the cache, then call `recompute`. The rebuilt row must
    // match bit-for-bit (stability, difficulty, due_at, reps, lapses)
    // — otherwise the recovery path doesn't actually preserve state.
    let tmp = TempDir::new().unwrap();
    let conn = common::fresh_db(&tmp, PATH_ID).await;

    let t0 = Utc::now() - Duration::days(60);
    let ratings = [
        (Rating::Good, t0),
        (Rating::Hard, t0 + Duration::days(10)),
        (Rating::Again, t0 + Duration::days(20)),
        (Rating::Good, t0 + Duration::days(25)),
    ];
    for (rating, ts) in ratings {
        let event = Event {
            ts,
            kind: EventKind::QuizAnswered,
            path: PATH_ID.into(),
            atom: Some("atom.x".into()),
            quiz: Some("atom.x.q1".into()),
            payload: EventPayload {
                rating: Some(rating),
                ..Default::default()
            },
        };
        event_log::append(&conn, &event).await.unwrap();
    }

    let before = cards::read_card(&conn, PATH_ID, "atom.x.q1")
        .await
        .unwrap()
        .expect("live cache row");

    cards::recompute(&conn, PATH_ID).await.unwrap();
    let after = cards::read_card(&conn, PATH_ID, "atom.x.q1")
        .await
        .unwrap()
        .expect("rebuilt cache row");

    assert!(
        (before.state.stability - after.state.stability).abs() < 1e-6,
        "stability {} vs {}",
        before.state.stability,
        after.state.stability
    );
    assert!((before.state.difficulty - after.state.difficulty).abs() < 1e-6);
    assert_eq!(before.state.due, after.state.due);
    assert_eq!(before.state.last_review, after.state.last_review);
    assert_eq!(before.reps, after.reps);
    assert_eq!(before.lapses, after.lapses);
}

#[tokio::test]
async fn dropped_transaction_rolls_back_event_and_cards() {
    // The event-insert + cards write-through must commit-or-fail
    // together. We exercise that by opening a transaction, appending
    // a `QuizAnswered`, and dropping the transaction without commit —
    // both tables must remain empty for the path afterwards.
    let tmp = TempDir::new().unwrap();
    let conn = common::fresh_db(&tmp, PATH_ID).await;
    {
        let tx = conn.transaction().await.unwrap();
        event_log::append(
            &tx,
            &event_log::quiz_answered(
                PATH_ID.into(),
                Some("atom.x".into()),
                "atom.x.q1".into(),
                Rating::Good,
                None,
            ),
        )
        .await
        .unwrap();
        // tx dropped without commit → automatic rollback.
    }

    let events = event_log::load(&conn, PATH_ID).await.unwrap();
    assert!(events.is_empty(), "events row must roll back with the tx");
    let card = cards::read_card(&conn, PATH_ID, "atom.x.q1").await.unwrap();
    assert!(card.is_none(), "cards row must roll back with the tx");
}

#[tokio::test]
async fn recompute_only_touches_target_path() {
    // Recompute must scope its delete to the named path. A second
    // path's cache should survive untouched.
    let tmp = TempDir::new().unwrap();
    let conn = common::fresh_db(&tmp, PATH_ID).await;
    conn.execute(
        "INSERT INTO paths(id, goal, created_at) VALUES (?, ?, ?)",
        params!["p_other", "other", "2026-05-26T00:00:00Z"],
    )
    .await
    .unwrap();

    event_log::append(
        &conn,
        &event_log::quiz_answered(
            PATH_ID.into(),
            Some("atom.x".into()),
            "atom.x.q1".into(),
            Rating::Good,
            None,
        ),
    )
    .await
    .unwrap();
    event_log::append(
        &conn,
        &event_log::quiz_answered(
            "p_other".into(),
            Some("atom.y".into()),
            "atom.y.q1".into(),
            Rating::Good,
            None,
        ),
    )
    .await
    .unwrap();

    cards::recompute(&conn, PATH_ID).await.unwrap();

    assert!(
        cards::read_card(&conn, PATH_ID, "atom.x.q1")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        cards::read_card(&conn, "p_other", "atom.y.q1")
            .await
            .unwrap()
            .is_some(),
        "other path's cache row must survive recompute"
    );
}
