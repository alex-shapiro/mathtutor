//! Integration tests for `PathProgress::load` — the SQL path that lets
//! `mt path state` (and the MCP `get_state` tool) build its progress
//! snapshot without materializing the full event log.
//!
//! The cards-backed predicate `reps > lapses` is supposed to be the SQL
//! equivalent of the event-derived "answered correctly at least once"
//! check. These tests pin that equivalence: every assertion either
//! exercises the cards table (write-through cache populated by
//! `event_log::append`) or the indexed lesson projection.

use libsql::{Connection, params};
use mathtutor::db::{self, DbConfig};
use mathtutor::event_log;
use mathtutor::progress::PathProgress;
use mathtutor::types::Rating;
use tempfile::TempDir;

const PATH_ID: &str = "p_test";

async fn fresh_db(dir: &TempDir) -> Connection {
    let cfg = DbConfig::local(dir.path().join("mt.db"));
    let db = db::open(&cfg).await.expect("open");
    let conn = db::connect(&db).await.expect("connect");
    conn.execute(
        "INSERT INTO paths(id, goal, created_at) VALUES (?, ?, ?)",
        params![PATH_ID, "test goal", "2026-05-26T00:00:00Z"],
    )
    .await
    .expect("seed path");
    conn
}

#[tokio::test]
async fn load_empty_path_yields_default_progress() {
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;

    let p = PathProgress::load(&conn, PATH_ID).await.expect("load");
    assert!(p.taught_atoms.is_empty());
    assert!(p.correct_quizzes.is_empty());
}

#[tokio::test]
async fn lesson_taught_event_lands_in_taught_atoms() {
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;
    event_log::append(
        &conn,
        &event_log::lesson_taught(PATH_ID.into(), "atom.a".into()),
    )
    .await
    .unwrap();

    let p = PathProgress::load(&conn, PATH_ID).await.unwrap();
    assert!(p.lesson_taught("atom.a"));
    assert!(!p.lesson_taught("atom.b"));
}

#[tokio::test]
async fn lesson_authored_event_also_counts_as_taught() {
    // Authoring a lesson implies presenting it (the `lesson upsert`
    // playbook). `PathProgress` must recognize either kind.
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;
    event_log::append(
        &conn,
        &event_log::lesson_authored(PATH_ID.into(), "atom.a".into()),
    )
    .await
    .unwrap();

    let p = PathProgress::load(&conn, PATH_ID).await.unwrap();
    assert!(p.lesson_taught("atom.a"));
}

#[tokio::test]
async fn correct_answer_lands_in_correct_quizzes() {
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;
    event_log::append(
        &conn,
        &event_log::quiz_answered(
            PATH_ID.into(),
            Some("atom.a".into()),
            "atom.a.q1".into(),
            Rating::Good,
            None,
        ),
    )
    .await
    .unwrap();

    let p = PathProgress::load(&conn, PATH_ID).await.unwrap();
    assert!(p.quiz_answered_correctly("atom.a.q1"));
}

#[tokio::test]
async fn only_again_answers_do_not_count_as_correct() {
    // `Again` alone keeps `lapses == reps`, so `reps > lapses` is false
    // — the cards row exists but the quiz is not "answered correctly."
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;
    event_log::append(
        &conn,
        &event_log::quiz_answered(
            PATH_ID.into(),
            Some("atom.a".into()),
            "atom.a.q1".into(),
            Rating::Again,
            None,
        ),
    )
    .await
    .unwrap();

    let p = PathProgress::load(&conn, PATH_ID).await.unwrap();
    assert!(!p.quiz_answered_correctly("atom.a.q1"));
}

#[tokio::test]
async fn earlier_correct_answer_survives_later_again() {
    // Get it right, then later get it wrong: the quiz still counts as
    // "answered correctly at least once" — `reps` grew by 2, `lapses`
    // grew by 1, so `reps > lapses` still holds.
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;
    event_log::append(
        &conn,
        &event_log::quiz_answered(
            PATH_ID.into(),
            Some("atom.a".into()),
            "atom.a.q1".into(),
            Rating::Good,
            None,
        ),
    )
    .await
    .unwrap();
    event_log::append(
        &conn,
        &event_log::quiz_answered(
            PATH_ID.into(),
            Some("atom.a".into()),
            "atom.a.q1".into(),
            Rating::Again,
            None,
        ),
    )
    .await
    .unwrap();

    let p = PathProgress::load(&conn, PATH_ID).await.unwrap();
    assert!(p.quiz_answered_correctly("atom.a.q1"));
}

#[tokio::test]
async fn loads_only_rows_for_the_named_path() {
    // A second path's events and cards must not leak into the snapshot.
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;
    conn.execute(
        "INSERT INTO paths(id, goal, created_at) VALUES (?, ?, ?)",
        params!["p_other", "other goal", "2026-05-26T00:00:00Z"],
    )
    .await
    .unwrap();

    event_log::append(
        &conn,
        &event_log::lesson_taught("p_other".into(), "atom.other".into()),
    )
    .await
    .unwrap();
    event_log::append(
        &conn,
        &event_log::quiz_answered(
            "p_other".into(),
            Some("atom.other".into()),
            "atom.other.q1".into(),
            Rating::Good,
            None,
        ),
    )
    .await
    .unwrap();

    let p = PathProgress::load(&conn, PATH_ID).await.unwrap();
    assert!(!p.lesson_taught("atom.other"));
    assert!(!p.quiz_answered_correctly("atom.other.q1"));
}
