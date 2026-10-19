//! Integration tests for `store` commands.

use mathtutor::db::Db;
use mathtutor::event_log::{self, EventKind};
use mathtutor::store;
use mathtutor::types::{Difficulty, QuizType, Strategy};

mod common;

const NO_LESSON_ATOM: &str = "fnd.1.1.2";
const WITH_LESSON_ATOM: &str = "fnd.1.1.1";

async fn fresh_db_with_path() -> (Db, String) {
    let db = common::empty_db().await;
    common::seed_path(&db, common::PATH_ID, &[NO_LESSON_ATOM], Strategy::BottomUp).await;
    (db, common::PATH_ID.into())
}

async fn event_kinds(db: &Db, path_id: &str) -> Vec<EventKind> {
    event_log::load(db, path_id)
        .await
        .expect("load events")
        .iter()
        .map(|e| e.kind)
        .filter(|k| *k != EventKind::PathCreated)
        .collect()
}

#[tokio::test]
async fn first_store_on_lesson_less_atom_emits_authored_then_taught() {
    let (db, path_id) = fresh_db_with_path().await;

    store::cmd_lesson_upsert(&db, NO_LESSON_ATOM, "body".into(), Some(&path_id), None)
        .await
        .expect("store");

    assert_eq!(
        event_kinds(&db, &path_id).await,
        vec![EventKind::LessonAuthored, EventKind::LessonTaught],
    );
}

#[tokio::test]
async fn second_store_on_same_atom_emits_amended_then_taught() {
    let (db, path_id) = fresh_db_with_path().await;

    store::cmd_lesson_upsert(&db, NO_LESSON_ATOM, "first".into(), Some(&path_id), None)
        .await
        .unwrap();
    store::cmd_lesson_upsert(&db, NO_LESSON_ATOM, "second".into(), Some(&path_id), None)
        .await
        .expect("second store succeeds (upsert)");

    assert_eq!(
        event_kinds(&db, &path_id).await,
        vec![
            EventKind::LessonAuthored,
            EventKind::LessonTaught,
            EventKind::LessonAmended,
            EventKind::LessonTaught,
        ],
    );
}

#[tokio::test]
async fn quiz_create_after_delete_skips_tombstoned_id() {
    let (db, path_id) = fresh_db_with_path().await;

    store::cmd_lesson_upsert(&db, NO_LESSON_ATOM, "body".into(), Some(&path_id), None)
        .await
        .unwrap();
    let first = store::cmd_quiz_create(
        &db,
        NO_LESSON_ATOM,
        Difficulty::Easy,
        "q1?".into(),
        "a1".into(),
        None,
        QuizType::FreeText,
        Some(&path_id),
        None,
    )
    .await
    .unwrap();
    assert_eq!(first, format!("{NO_LESSON_ATOM}.q1"));

    store::cmd_quiz_delete(&db, &first, Some(&path_id), None)
        .await
        .unwrap();

    let second = store::cmd_quiz_create(
        &db,
        NO_LESSON_ATOM,
        Difficulty::Easy,
        "q2?".into(),
        "a2".into(),
        None,
        QuizType::FreeText,
        Some(&path_id),
        None,
    )
    .await
    .expect("create after delete must not recycle the tombstoned id");
    assert_eq!(second, format!("{NO_LESSON_ATOM}.q2"));
}

#[tokio::test]
async fn store_on_shipped_lesson_atom_emits_amended() {
    let (db, path_id) = fresh_db_with_path().await;

    store::cmd_lesson_upsert(
        &db,
        WITH_LESSON_ATOM,
        "override".into(),
        Some(&path_id),
        None,
    )
    .await
    .expect("store");

    assert_eq!(
        event_kinds(&db, &path_id).await,
        vec![EventKind::LessonAmended, EventKind::LessonTaught],
    );
}
