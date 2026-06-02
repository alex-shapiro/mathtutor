//! Tests for the SQL-backed history aggregates used by `present_quiz`
//! / `present_lesson` envelopes: `scheduler::compute_quiz_history` and
//! `scheduler::compute_lesson_history`.
//!
//! Each test seeds an in-memory libSQL database, appends events, then
//! asserts on the aggregates returned by the helpers — without going
//! through `compute_next`'s walker (which would otherwise need a full
//! prereq tree seeded for any embedded-curriculum target).

use chrono::{Duration, TimeZone, Utc};
use libsql::params;
use mathtutor::event_log::{self, Event, EventKind, EventPayload};
use mathtutor::scheduler;
use mathtutor::types::Rating;
use tempfile::TempDir;

mod common;

const PATH_ID: &str = "p_test";
const ATOM: &str = "test.atom";
const QUIZ: &str = "test.atom.q1";

fn presented(quiz_id: &str, ts: chrono::DateTime<Utc>) -> Event {
    Event {
        ts,
        kind: EventKind::QuizPresented,
        path: PATH_ID.into(),
        atom: Some(ATOM.into()),
        quiz: Some(quiz_id.into()),
        payload: EventPayload::default(),
    }
}

fn answered(quiz_id: &str, rating: Rating, ts: chrono::DateTime<Utc>) -> Event {
    Event {
        ts,
        kind: EventKind::QuizAnswered,
        path: PATH_ID.into(),
        atom: Some(ATOM.into()),
        quiz: Some(quiz_id.into()),
        payload: EventPayload {
            rating: Some(rating),
            ..Default::default()
        },
    }
}

fn taught(atom_id: &str, ts: chrono::DateTime<Utc>) -> Event {
    Event {
        ts,
        kind: EventKind::LessonTaught,
        path: PATH_ID.into(),
        atom: Some(atom_id.into()),
        quiz: None,
        payload: EventPayload::default(),
    }
}

// ── compute_quiz_history ────────────────────────────────────────────

#[tokio::test]
async fn quiz_history_empty_when_quiz_never_touched() {
    let tmp = TempDir::new().unwrap();
    let conn = common::fresh_db(&tmp, PATH_ID).await;

    let h = scheduler::compute_quiz_history(&conn, PATH_ID, QUIZ)
        .await
        .unwrap();
    assert_eq!(h.repetitions, 0);
    assert_eq!(h.total_count, 0);
    assert_eq!(h.correct_count, 0);
    assert_eq!(h.correct_pct, None);
    assert!(h.last_presented_at.is_none());
    assert!(h.recent_ratings.is_empty());
}

#[tokio::test]
async fn quiz_history_counts_presentations_and_picks_latest_ts() {
    let tmp = TempDir::new().unwrap();
    let conn = common::fresh_db(&tmp, PATH_ID).await;

    let t0 = Utc.with_ymd_and_hms(2026, 5, 26, 0, 0, 0).unwrap();
    let t1 = t0 + Duration::minutes(5);
    let t2 = t0 + Duration::minutes(15);
    event_log::append(&conn, &presented(QUIZ, t0))
        .await
        .unwrap();
    event_log::append(&conn, &presented(QUIZ, t1))
        .await
        .unwrap();
    event_log::append(&conn, &presented(QUIZ, t2))
        .await
        .unwrap();

    let h = scheduler::compute_quiz_history(&conn, PATH_ID, QUIZ)
        .await
        .unwrap();
    assert_eq!(h.repetitions, 3);
    assert_eq!(h.last_presented_at, Some(t2));
}

#[tokio::test]
async fn quiz_history_answer_aggregates_match_cards_cache() {
    // Three answers: Good, Again, Easy → reps=3, lapses=1.
    // correct_count = reps - lapses = 2; correct_pct = round(2/3 * 100) = 67.
    let tmp = TempDir::new().unwrap();
    let conn = common::fresh_db(&tmp, PATH_ID).await;

    let t0 = Utc.with_ymd_and_hms(2026, 5, 26, 0, 0, 0).unwrap();
    event_log::append(&conn, &answered(QUIZ, Rating::Good, t0))
        .await
        .unwrap();
    event_log::append(
        &conn,
        &answered(QUIZ, Rating::Again, t0 + Duration::minutes(1)),
    )
    .await
    .unwrap();
    event_log::append(
        &conn,
        &answered(QUIZ, Rating::Easy, t0 + Duration::minutes(2)),
    )
    .await
    .unwrap();

    let h = scheduler::compute_quiz_history(&conn, PATH_ID, QUIZ)
        .await
        .unwrap();
    assert_eq!(h.total_count, 3);
    assert_eq!(h.correct_count, 2);
    assert_eq!(h.correct_pct, Some(67));
}

#[tokio::test]
async fn quiz_history_recent_ratings_newest_first_capped_at_ten() {
    let tmp = TempDir::new().unwrap();
    let conn = common::fresh_db(&tmp, PATH_ID).await;

    let t0 = Utc.with_ymd_and_hms(2026, 5, 26, 0, 0, 0).unwrap();
    // 12 answers in chronological order — only the last 10 should come
    // back, newest first. The 11th-from-last is dropped.
    let ratings = [
        Rating::Good,  // dropped (oldest)
        Rating::Good,  // dropped
        Rating::Again, // → position 9 (oldest kept)
        Rating::Hard,
        Rating::Good,
        Rating::Easy,
        Rating::Good,
        Rating::Again,
        Rating::Good,
        Rating::Hard,
        Rating::Easy,
        Rating::Good, // → position 0 (newest)
    ];
    for (i, r) in ratings.iter().enumerate() {
        let ts = t0 + Duration::seconds(i64::try_from(i).unwrap());
        event_log::append(&conn, &answered(QUIZ, *r, ts))
            .await
            .unwrap();
    }

    let h = scheduler::compute_quiz_history(&conn, PATH_ID, QUIZ)
        .await
        .unwrap();
    assert_eq!(h.recent_ratings.len(), 10);
    assert_eq!(h.recent_ratings[0], Rating::Good, "newest first");
    assert_eq!(
        h.recent_ratings[9],
        Rating::Again,
        "oldest kept = third-from-last in input",
    );
}

#[tokio::test]
async fn quiz_history_isolates_quiz_id_and_path_id() {
    // Events for a different quiz, and for a different path, must not
    // leak into the aggregates.
    let tmp = TempDir::new().unwrap();
    let conn = common::fresh_db(&tmp, PATH_ID).await;
    conn.execute(
        "INSERT INTO paths(id, goal, created_at) VALUES (?, ?, ?)",
        params!["p_other", "g", "2026-05-26T00:00:00Z"],
    )
    .await
    .unwrap();

    let t0 = Utc.with_ymd_and_hms(2026, 5, 26, 0, 0, 0).unwrap();
    event_log::append(&conn, &presented("other.q1", t0))
        .await
        .unwrap();
    event_log::append(
        &conn,
        &Event {
            ts: t0,
            kind: EventKind::QuizAnswered,
            path: "p_other".into(),
            atom: Some(ATOM.into()),
            quiz: Some(QUIZ.into()),
            payload: EventPayload {
                rating: Some(Rating::Good),
                ..Default::default()
            },
        },
    )
    .await
    .unwrap();

    let h = scheduler::compute_quiz_history(&conn, PATH_ID, QUIZ)
        .await
        .unwrap();
    assert_eq!(h.repetitions, 0, "other.q1 presentation must not leak");
    assert_eq!(h.total_count, 0, "p_other's answer must not leak");
}

// ── compute_lesson_history ──────────────────────────────────────────

#[tokio::test]
async fn lesson_history_empty_when_atom_never_taught() {
    let tmp = TempDir::new().unwrap();
    let conn = common::fresh_db(&tmp, PATH_ID).await;

    let h = scheduler::compute_lesson_history(&conn, PATH_ID, ATOM)
        .await
        .unwrap();
    assert_eq!(h.repetitions, 0);
    assert!(h.last_presented_at.is_none());
}

#[tokio::test]
async fn lesson_history_counts_taught_events_and_picks_latest_ts() {
    let tmp = TempDir::new().unwrap();
    let conn = common::fresh_db(&tmp, PATH_ID).await;

    let t0 = Utc.with_ymd_and_hms(2026, 5, 26, 0, 0, 0).unwrap();
    let t1 = t0 + Duration::days(1);
    let t2 = t0 + Duration::days(3);
    event_log::append(&conn, &taught(ATOM, t0)).await.unwrap();
    event_log::append(&conn, &taught(ATOM, t1)).await.unwrap();
    event_log::append(&conn, &taught(ATOM, t2)).await.unwrap();

    let h = scheduler::compute_lesson_history(&conn, PATH_ID, ATOM)
        .await
        .unwrap();
    assert_eq!(h.repetitions, 3);
    assert_eq!(h.last_presented_at, Some(t2));
}

#[tokio::test]
async fn lesson_history_isolates_atom_and_path() {
    let tmp = TempDir::new().unwrap();
    let conn = common::fresh_db(&tmp, PATH_ID).await;
    conn.execute(
        "INSERT INTO paths(id, goal, created_at) VALUES (?, ?, ?)",
        params!["p_other", "g", "2026-05-26T00:00:00Z"],
    )
    .await
    .unwrap();

    let t0 = Utc.with_ymd_and_hms(2026, 5, 26, 0, 0, 0).unwrap();
    event_log::append(&conn, &taught("other.atom", t0))
        .await
        .unwrap();
    event_log::append(
        &conn,
        &Event {
            ts: t0,
            kind: EventKind::LessonTaught,
            path: "p_other".into(),
            atom: Some(ATOM.into()),
            quiz: None,
            payload: EventPayload::default(),
        },
    )
    .await
    .unwrap();

    let h = scheduler::compute_lesson_history(&conn, PATH_ID, ATOM)
        .await
        .unwrap();
    assert_eq!(h.repetitions, 0);
}
