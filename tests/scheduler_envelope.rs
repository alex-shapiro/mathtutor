//! Tests for the history aggregates used by `present_quiz` /
//! `present_lesson` envelopes: `scheduler::quiz_history` and
//! `scheduler::lesson_history`.
//!
//! Each test seeds a fresh Baserunna database, appends events, then
//! asserts on the aggregates computed from the stored cards and taught
//! rows — without going
//! through `compute_next`'s walker (which would otherwise need a full
//! prereq tree seeded for any embedded-curriculum target).

use chrono::{Duration, TimeZone, Utc};
use mathtutor::db::Db;
use mathtutor::event_log::{Event, EventKind, EventPayload};
use mathtutor::scheduler::{self, LessonHistory, QuizHistory};
use mathtutor::types::{Rating, Strategy};

mod common;

use common::{PATH_ID, answered_at, presented_at, taught_at};

const ATOM: &str = "test.atom";
const QUIZ: &str = "test.atom.q1";

async fn quiz_history(db: &Db) -> QuizHistory {
    scheduler::quiz_history(&common::path_state(db, PATH_ID).await, QUIZ)
}

async fn lesson_history(db: &Db) -> LessonHistory {
    scheduler::lesson_history(&common::path_state(db, PATH_ID).await, ATOM)
}

// ── compute_quiz_history ────────────────────────────────────────────

#[tokio::test]
async fn quiz_history_empty_when_quiz_never_touched() {
    let db = common::fresh_db(PATH_ID).await;

    let h = quiz_history(&db).await;
    assert_eq!(h.repetitions, 0);
    assert_eq!(h.total_count, 0);
    assert_eq!(h.correct_count, 0);
    assert_eq!(h.correct_pct, None);
    assert!(h.last_presented_at.is_none());
    assert!(h.recent_ratings.is_empty());
}

#[tokio::test]
async fn quiz_history_counts_presentations_and_picks_latest_ts() {
    let db = common::fresh_db(PATH_ID).await;

    let t0 = Utc.with_ymd_and_hms(2026, 5, 26, 0, 0, 0).unwrap();
    let t1 = t0 + Duration::minutes(5);
    let t2 = t0 + Duration::minutes(15);
    common::append(&db, &[presented_at(QUIZ, t0)]).await;
    common::append(&db, &[presented_at(QUIZ, t1)]).await;
    common::append(&db, &[presented_at(QUIZ, t2)]).await;

    let h = quiz_history(&db).await;
    assert_eq!(h.repetitions, 3);
    assert_eq!(h.last_presented_at, Some(t2));
}

#[tokio::test]
async fn quiz_history_answer_aggregates_match_cards_cache() {
    // Three answers: Good, Again, Easy → reps=3, lapses=1.
    // correct_count = reps - lapses = 2; correct_pct = round(2/3 * 100) = 67.
    let db = common::fresh_db(PATH_ID).await;

    let t0 = Utc.with_ymd_and_hms(2026, 5, 26, 0, 0, 0).unwrap();
    common::append(&db, &[answered_at(QUIZ, Rating::Good, t0)]).await;
    common::append(
        &db,
        &[answered_at(QUIZ, Rating::Again, t0 + Duration::minutes(1))],
    )
    .await;
    common::append(
        &db,
        &[answered_at(QUIZ, Rating::Easy, t0 + Duration::minutes(2))],
    )
    .await;

    let h = quiz_history(&db).await;
    assert_eq!(h.total_count, 3);
    assert_eq!(h.correct_count, 2);
    assert_eq!(h.correct_pct, Some(67));
}

#[tokio::test]
async fn quiz_history_recent_ratings_newest_first_capped_at_ten() {
    let db = common::fresh_db(PATH_ID).await;

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
        common::append(&db, &[answered_at(QUIZ, *r, ts)]).await;
    }

    let h = quiz_history(&db).await;
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
    let db = common::fresh_db(PATH_ID).await;
    common::seed_path(&db, "p_other", &[], Strategy::BottomUp).await;

    let t0 = Utc.with_ymd_and_hms(2026, 5, 26, 0, 0, 0).unwrap();
    common::append(&db, &[presented_at("other.q1", t0)]).await;
    common::append(
        &db,
        &[Event {
            ts: t0,
            kind: EventKind::QuizAnswered,
            path: "p_other".into(),
            atom: Some(ATOM.into()),
            quiz: Some(QUIZ.into()),
            payload: EventPayload {
                rating: Some(Rating::Good),
                ..Default::default()
            },
        }],
    )
    .await;

    let h = quiz_history(&db).await;
    assert_eq!(h.repetitions, 0, "other.q1 presentation must not leak");
    assert_eq!(h.total_count, 0, "p_other's answer must not leak");
}

// ── compute_lesson_history ──────────────────────────────────────────

#[tokio::test]
async fn lesson_history_empty_when_atom_never_taught() {
    let db = common::fresh_db(PATH_ID).await;

    let h = lesson_history(&db).await;
    assert_eq!(h.repetitions, 0);
    assert!(h.last_presented_at.is_none());
}

#[tokio::test]
async fn lesson_history_counts_taught_events_and_picks_latest_ts() {
    let db = common::fresh_db(PATH_ID).await;

    let t0 = Utc.with_ymd_and_hms(2026, 5, 26, 0, 0, 0).unwrap();
    let t1 = t0 + Duration::days(1);
    let t2 = t0 + Duration::days(3);
    common::append(&db, &[taught_at(ATOM, t0)]).await;
    common::append(&db, &[taught_at(ATOM, t1)]).await;
    common::append(&db, &[taught_at(ATOM, t2)]).await;

    let h = lesson_history(&db).await;
    assert_eq!(h.repetitions, 3);
    assert_eq!(h.last_presented_at, Some(t2));
}

#[tokio::test]
async fn lesson_history_isolates_atom_and_path() {
    let db = common::fresh_db(PATH_ID).await;
    common::seed_path(&db, "p_other", &[], Strategy::BottomUp).await;

    let t0 = Utc.with_ymd_and_hms(2026, 5, 26, 0, 0, 0).unwrap();
    common::append(&db, &[taught_at("other.atom", t0)]).await;
    common::append(
        &db,
        &[Event {
            ts: t0,
            kind: EventKind::LessonTaught,
            path: "p_other".into(),
            atom: Some(ATOM.into()),
            quiz: None,
            payload: EventPayload::default(),
        }],
    )
    .await;

    let h = lesson_history(&db).await;
    assert_eq!(h.repetitions, 0);
}
