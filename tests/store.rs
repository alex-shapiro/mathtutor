//! Integration tests for `store::cmd_lesson_upsert`.
//!
//! These pin the upsert contract: the first call for an atom emits
//! `lesson_authored`, subsequent calls emit `lesson_amended`, and
//! every call emits `lesson_taught` since storing implies presenting.

use chrono::Utc;
use libsql::Connection;
use mathtutor::db::{self, DbConfig};
use mathtutor::event_log::{self, EventKind};
use mathtutor::path::{self, PathFile};
use mathtutor::store;
use tempfile::TempDir;

/// Atom that ships *without* a lesson — exercises the
/// `lesson_authored` branch on the first store.
const NO_LESSON_ATOM: &str = "fnd.1.1.2";
/// Atom that ships *with* a lesson — exercises the `lesson_amended`
/// branch even on the first store.
const WITH_LESSON_ATOM: &str = "fnd.1.1.1";

async fn fresh_db_with_path(dir: &TempDir) -> (Connection, String) {
    let cfg = DbConfig::local(dir.path().join("mt.db"));
    let db = db::open(&cfg).await.expect("open");
    let conn = db::connect(&db).await.expect("connect");
    let p = PathFile {
        id: "p_test".into(),
        goal: "test".into(),
        created_at: Utc::now(),
        target_atoms: vec![NO_LESSON_ATOM.into()],
    };
    path::save_path(&conn, &p).await.expect("save_path");
    (conn, p.id)
}

async fn event_kinds(conn: &Connection, path_id: &str) -> Vec<EventKind> {
    event_log::load(conn, path_id)
        .await
        .expect("load events")
        .iter()
        .map(|e| e.kind)
        .collect()
}

#[tokio::test]
async fn first_store_on_lesson_less_atom_emits_authored_then_taught() {
    let tmp = TempDir::new().unwrap();
    let (conn, path_id) = fresh_db_with_path(&tmp).await;

    store::cmd_lesson_upsert(&conn, NO_LESSON_ATOM, "body".into(), Some(&path_id), None)
        .await
        .expect("store");

    assert_eq!(
        event_kinds(&conn, &path_id).await,
        vec![EventKind::LessonAuthored, EventKind::LessonTaught],
    );
}

#[tokio::test]
async fn second_store_on_same_atom_emits_amended_then_taught() {
    // Upsert semantics: the SQL row is replaced, and the event log
    // distinguishes the follow-up store from the initial authoring.
    let tmp = TempDir::new().unwrap();
    let (conn, path_id) = fresh_db_with_path(&tmp).await;

    store::cmd_lesson_upsert(&conn, NO_LESSON_ATOM, "first".into(), Some(&path_id), None)
        .await
        .unwrap();
    store::cmd_lesson_upsert(&conn, NO_LESSON_ATOM, "second".into(), Some(&path_id), None)
        .await
        .expect("second store succeeds (upsert)");

    assert_eq!(
        event_kinds(&conn, &path_id).await,
        vec![
            EventKind::LessonAuthored,
            EventKind::LessonTaught,
            EventKind::LessonAmended,
            EventKind::LessonTaught,
        ],
    );
}

#[tokio::test]
async fn store_on_shipped_lesson_atom_emits_amended() {
    // The atom already has a lesson in the shipped curriculum, so
    // even the first store is an amendment from the user's POV.
    let tmp = TempDir::new().unwrap();
    let (conn, path_id) = fresh_db_with_path(&tmp).await;

    store::cmd_lesson_upsert(
        &conn,
        WITH_LESSON_ATOM,
        "override".into(),
        Some(&path_id),
        None,
    )
    .await
    .expect("store");

    assert_eq!(
        event_kinds(&conn, &path_id).await,
        vec![EventKind::LessonAmended, EventKind::LessonTaught],
    );
}
