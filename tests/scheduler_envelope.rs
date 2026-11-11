//! Integration tests for the `mt path next` envelope, `scheduler::quiz_history`, and `scheduler::lesson_history`.

use chrono::{Duration, TimeZone, Utc};
use mathtutor::db::Db;
use mathtutor::event_log::Event;
use mathtutor::overlay::QuizDraft;
use mathtutor::scheduler::{self, LessonHistory, QuizHistory};
use mathtutor::surface::Surface;
use mathtutor::types::{Difficulty, QuizType, Rating, Strategy};
use mathtutor::{path, store};

mod common;

use common::{PATH_ID, answered, at, presented, taught};

const ATOM: &str = "test.atom";
const QUIZ: &str = "test.atom.q1";

async fn quiz_history(db: &Db) -> QuizHistory {
    scheduler::quiz_history(&common::path_state(db, PATH_ID).await, QUIZ)
}

async fn lesson_history(db: &Db) -> LessonHistory {
    scheduler::lesson_history(&common::path_state(db, PATH_ID).await, ATOM)
}

#[tokio::test]
async fn quiz_history_empty_if_quiz_unanswered() {
    let db = common::fresh_db(PATH_ID).await;

    let h = quiz_history(&db).await;
    assert_eq!(h.repetitions, 0);
    assert_eq!(h.total_count, 0);
    assert_eq!(h.correct_count, 0);
    assert_eq!(h.correct_pct, None);
    assert!(h.last_presented_at.is_none());
    assert_eq!(h.recent_ratings, Vec::<Rating>::new());
}

#[tokio::test]
async fn quiz_history_counts_presentations_and_picks_latest_ts() {
    let db = common::fresh_db(PATH_ID).await;

    let t0 = Utc.with_ymd_and_hms(2026, 5, 26, 0, 0, 0).unwrap();
    let t1 = t0 + Duration::minutes(5);
    let t2 = t0 + Duration::minutes(15);
    common::append(&db, &[at(t0, presented(QUIZ))]).await;
    common::append(&db, &[at(t1, presented(QUIZ))]).await;
    common::append(&db, &[at(t2, presented(QUIZ))]).await;

    let h = quiz_history(&db).await;
    assert_eq!(h.repetitions, 3);
    assert_eq!(h.last_presented_at, Some(t2));
}

#[tokio::test]
async fn quiz_history_answer_aggregates_match_cards_cache() {
    let db = common::fresh_db(PATH_ID).await;

    let t0 = Utc.with_ymd_and_hms(2026, 5, 26, 0, 0, 0).unwrap();
    common::append(&db, &[at(t0, answered(QUIZ, Rating::Good))]).await;
    common::append(
        &db,
        &[at(t0 + Duration::minutes(1), answered(QUIZ, Rating::Again))],
    )
    .await;
    common::append(
        &db,
        &[at(t0 + Duration::minutes(2), answered(QUIZ, Rating::Easy))],
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
    let ratings = [
        Rating::Good,
        Rating::Good,
        Rating::Again,
        Rating::Hard,
        Rating::Good,
        Rating::Easy,
        Rating::Good,
        Rating::Again,
        Rating::Good,
        Rating::Hard,
        Rating::Easy,
        Rating::Good,
    ];
    for (i, r) in ratings.iter().enumerate() {
        let ts = t0 + Duration::seconds(i64::try_from(i).unwrap());
        common::append(&db, &[at(ts, answered(QUIZ, *r))]).await;
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
    let db = common::fresh_db(PATH_ID).await;
    common::seed_path(&db, "p_other", &[], Strategy::BottomUp).await;

    let t0 = Utc.with_ymd_and_hms(2026, 5, 26, 0, 0, 0).unwrap();
    common::append(&db, &[at(t0, presented("other.q1"))]).await;
    common::append(
        &db,
        &[Event {
            path: "p_other".into(),
            ..at(t0, answered(QUIZ, Rating::Good))
        }],
    )
    .await;

    let h = quiz_history(&db).await;
    assert_eq!(h.repetitions, 0, "other.q1 presentation must not leak");
    assert_eq!(h.total_count, 0, "p_other's answer must not leak");
}

#[tokio::test]
async fn lesson_history_empty_if_atom_untaught() {
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
    common::append(&db, &[at(t0, taught(ATOM))]).await;
    common::append(&db, &[at(t1, taught(ATOM))]).await;
    common::append(&db, &[at(t2, taught(ATOM))]).await;

    let h = lesson_history(&db).await;
    assert_eq!(h.repetitions, 3);
    assert_eq!(h.last_presented_at, Some(t2));
}

#[tokio::test]
async fn lesson_history_isolates_atom_and_path() {
    let db = common::fresh_db(PATH_ID).await;
    common::seed_path(&db, "p_other", &[], Strategy::BottomUp).await;

    let t0 = Utc.with_ymd_and_hms(2026, 5, 26, 0, 0, 0).unwrap();
    common::append(&db, &[at(t0, taught("other.atom"))]).await;
    common::append(
        &db,
        &[Event {
            path: "p_other".into(),
            ..at(t0, taught(ATOM))
        }],
    )
    .await;

    let h = lesson_history(&db).await;
    assert_eq!(h.repetitions, 0);
}

async fn next_envelope(db: &Db, path_id: &str, surface: Surface) -> serde_json::Value {
    let envelope =
        scheduler::compute_next(db, Some(path_id), scheduler::NextMode::Default, surface)
            .await
            .expect("next");
    serde_json::to_value(envelope).expect("json")
}

#[tokio::test]
async fn next_step_includes_the_path_and_the_verbatim_answer_on_each_surface() {
    let db = common::empty_db().await;
    let id = path::cmd_path_new(&db, "g", &["fnd.1.1.2".into()], Strategy::TopDown)
        .await
        .unwrap();
    store::cmd_lesson_upsert(&db, "fnd.1.1.2", "body".into(), Some(&id))
        .await
        .unwrap();

    let create = next_envelope(&db, &id, Surface::Cli).await;
    assert_eq!(create["action"], "create_quiz");
    let step = create["next_step"].as_str().unwrap();
    assert!(
        step.starts_with("mt quiz create fnd.1.1.2 --path "),
        "{step}"
    );
    assert!(step.contains(&id), "{step}");
    let mcp_create = next_envelope(&db, &id, Surface::Mcp).await;
    let step = mcp_create["next_step"].as_str().unwrap();
    assert!(
        step.starts_with("create_quiz { atom: \"fnd.1.1.2\""),
        "{step}"
    );
    assert!(step.contains(&format!("path_id: \"{id}\"")), "{step}");

    let quiz = store::cmd_quiz_create(
        &db,
        "fnd.1.1.2",
        QuizDraft {
            difficulty: Difficulty::Easy,
            question: "q".into(),
            answer: "a".into(),
            rubric: None,
            kind: QuizType::FreeText,
        },
        Some(&id),
    )
    .await
    .unwrap();

    let present = next_envelope(&db, &id, Surface::Cli).await;
    assert_eq!(present["action"], "present_quiz");
    let step = present["next_step"].as_str().unwrap();
    assert!(
        step.starts_with(&format!("mt quiz answer {quiz} --path {id} --rating ")),
        "{step}"
    );
    assert!(step.contains("--user-answer"), "{step}");
    let mcp_present = next_envelope(&db, &id, Surface::Mcp).await;
    let step = mcp_present["next_step"].as_str().unwrap();
    assert!(
        step.starts_with(&format!(
            "answer_quiz {{ quiz_id: \"{quiz}\", path_id: \"{id}\""
        )),
        "{step}"
    );
    assert!(step.contains("user_answer: "), "{step}");
    assert!(!step.contains("mt "), "{step}");
}

#[tokio::test]
async fn due_mode_without_due_reviews_does_not_report_a_complete_path() {
    let db = common::empty_db().await;
    let id = path::cmd_path_new(&db, "g", &["fnd.1.1.2".into()], Strategy::BottomUp)
        .await
        .unwrap();
    let envelope = scheduler::compute_next(&db, Some(&id), scheduler::NextMode::Due, Surface::Cli)
        .await
        .unwrap();
    let envelope = serde_json::to_value(envelope).unwrap();
    assert_eq!(envelope["action"], "no_reviews_due");
    assert_eq!(envelope["next_step"], format!("mt path next --path {id}"));
    assert_ne!(envelope["payload"]["message"], "Path complete.");
}
