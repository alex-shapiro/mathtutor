//! Integration tests for `PathState::progress`.

use mathtutor::event_log::{self, Event};
use mathtutor::progress::PathProgress;
use mathtutor::types::{Rating, Strategy};

mod common;

use common::PATH_ID;

async fn progress(db: &mathtutor::db::Db, path_id: &str) -> PathProgress {
    common::path_state(db, path_id).await.progress()
}

fn answered(path: &str, atom: &str, rating: Rating) -> Event {
    event_log::quiz_answered(
        path.into(),
        Some(atom.into()),
        format!("{atom}.q1"),
        rating,
        None,
    )
}

#[tokio::test]
async fn load_empty_path_yields_default_progress() {
    let db = common::fresh_db(PATH_ID).await;
    let p = progress(&db, PATH_ID).await;
    assert!(p.taught_atoms.is_empty());
    assert!(p.correct_quizzes.is_empty());
}

#[tokio::test]
async fn lesson_taught_event_lands_in_taught_atoms() {
    let db = common::fresh_db(PATH_ID).await;
    common::append(
        &db,
        &[event_log::lesson_taught(PATH_ID.into(), "atom.a".into())],
    )
    .await;

    let p = progress(&db, PATH_ID).await;
    assert!(p.lesson_taught("atom.a"));
    assert!(!p.lesson_taught("atom.b"));
}

#[tokio::test]
async fn lesson_authored_event_also_counts_as_taught() {
    let db = common::fresh_db(PATH_ID).await;
    common::append(
        &db,
        &[event_log::lesson_authored(PATH_ID.into(), "atom.a".into())],
    )
    .await;

    let p = progress(&db, PATH_ID).await;
    assert!(p.lesson_taught("atom.a"));
}

#[tokio::test]
async fn correct_answer_lands_in_correct_quizzes() {
    let db = common::fresh_db(PATH_ID).await;
    common::append(&db, &[answered(PATH_ID, "atom.a", Rating::Good)]).await;

    let p = progress(&db, PATH_ID).await;
    assert!(p.quiz_answered_correctly("atom.a.q1"));
}

#[tokio::test]
async fn only_again_answers_do_not_count_as_correct() {
    let db = common::fresh_db(PATH_ID).await;
    common::append(&db, &[answered(PATH_ID, "atom.a", Rating::Again)]).await;

    let p = progress(&db, PATH_ID).await;
    assert!(!p.quiz_answered_correctly("atom.a.q1"));
}

#[tokio::test]
async fn earlier_correct_answer_survives_later_again() {
    let db = common::fresh_db(PATH_ID).await;
    common::append(&db, &[answered(PATH_ID, "atom.a", Rating::Good)]).await;
    common::append(&db, &[answered(PATH_ID, "atom.a", Rating::Again)]).await;

    let p = progress(&db, PATH_ID).await;
    assert!(p.quiz_answered_correctly("atom.a.q1"));
}

#[tokio::test]
async fn loads_only_rows_for_the_named_path() {
    let db = common::fresh_db(PATH_ID).await;
    common::seed_path(&db, "p_other", &[], Strategy::BottomUp).await;
    common::append(
        &db,
        &[
            event_log::lesson_taught("p_other".into(), "atom.other".into()),
            answered("p_other", "atom.other", Rating::Good),
        ],
    )
    .await;

    let p = progress(&db, PATH_ID).await;
    assert!(!p.lesson_taught("atom.other"));
    assert!(!p.quiz_answered_correctly("atom.other.q1"));

    let other = progress(&db, "p_other").await;
    assert!(other.lesson_taught("atom.other"));
    assert!(other.quiz_answered_correctly("atom.other.q1"));
}
