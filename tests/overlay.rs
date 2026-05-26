//! Integration tests for the SQL-backed global overlay.
//!
//! Each test opens a fresh local libSQL database in a `tempdir` and
//! drives `overlay::*` plus `Graph::load_for_path` to verify three
//! contracts:
//!
//! 1. CRUD operations write the expected rows to the overlay tables.
//! 2. `Graph::load_for_path` merges the global overlay on top of the
//!    shipped curriculum (lessons, amendments, tombstones, new quizzes).
//! 3. Overlays are global — a write made under one path is visible to
//!    every other path on the same database.

use libsql::{Connection, params};
use mathtutor::db::{self, DbConfig};
use mathtutor::graph::{Graph, QuizRaw};
use mathtutor::overlay;
use mathtutor::types::{Difficulty, QuizType};
use tempfile::TempDir;

/// Atom that ships *without* a lesson in `curriculum/graph` — used to
/// verify `add_lesson` writes new content rather than collide with the
/// shipped data.
const NO_LESSON_ATOM: &str = "fnd.1.1.2";
/// Atom that ships *with* a lesson and one quiz (`fnd.1.1.1.q1`) —
/// used to verify merge semantics for existing content.
const WITH_LESSON_ATOM: &str = "fnd.1.1.1";
const SHIPPED_QUIZ_ID: &str = "fnd.1.1.1.q1";

async fn fresh_db(dir: &TempDir) -> Connection {
    let cfg = DbConfig::local(dir.path().join("mt.db"));
    let db = db::open(&cfg).await.expect("open");
    db::connect(&db).await.expect("connect")
}

#[tokio::test]
async fn load_returns_empty_overlay_on_fresh_db() {
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;
    let ov = overlay::load(&conn).await.expect("load");
    assert!(ov.atoms.is_empty());
}

#[tokio::test]
async fn add_lesson_writes_row_and_round_trips() {
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;

    overlay::add_lesson(&conn, NO_LESSON_ATOM, "Negation flips truth.")
        .await
        .expect("add_lesson");

    let ov = overlay::load(&conn).await.expect("load");
    let entry = ov.atoms.get(NO_LESSON_ATOM).expect("atom present");
    assert_eq!(entry.lesson.as_deref(), Some("Negation flips truth."));
}

#[tokio::test]
async fn add_lesson_returns_already_exists_on_collision() {
    // The SQL primary key on `overlay_lessons.atom_id` is the
    // backstop: a second insert for the same atom must surface as
    // `LessonAlreadyExists`, not as a raw libSQL error.
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;

    overlay::add_lesson(&conn, NO_LESSON_ATOM, "first")
        .await
        .unwrap();
    let err = overlay::add_lesson(&conn, NO_LESSON_ATOM, "second")
        .await
        .expect_err("second insert must fail");
    assert!(matches!(
        err,
        mathtutor::Error::LessonAlreadyExists(ref id) if id == NO_LESSON_ATOM
    ));
}

#[tokio::test]
async fn add_quiz_writes_row_and_round_trips() {
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;

    let quiz = QuizRaw {
        id: "fnd.1.1.2.q1".into(),
        difficulty: Difficulty::Medium,
        kind: Some(QuizType::MultipleChoice),
        question: "Pick the negation".into(),
        answer: "¬P".into(),
        rubric: Some("Look for the ¬ glyph".into()),
    };
    overlay::add_quiz(&conn, NO_LESSON_ATOM, &quiz)
        .await
        .expect("add_quiz");

    let ov = overlay::load(&conn).await.expect("load");
    let entry = ov.atoms.get(NO_LESSON_ATOM).expect("atom present");
    assert_eq!(entry.quizzes.len(), 1);
    let stored = &entry.quizzes[0];
    assert_eq!(stored.id, "fnd.1.1.2.q1");
    assert_eq!(stored.difficulty, Difficulty::Medium);
    assert_eq!(stored.kind, Some(QuizType::MultipleChoice));
    assert_eq!(stored.question, "Pick the negation");
    assert_eq!(stored.answer, "¬P");
    assert_eq!(stored.rubric.as_deref(), Some("Look for the ¬ glyph"));
}

#[tokio::test]
async fn amend_quiz_upserts_and_untombstones() {
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;

    // Tombstone the shipped quiz, then amend it. The amend must
    // un-tombstone so the quiz comes back into the merged view.
    overlay::remove_quiz(&conn, SHIPPED_QUIZ_ID).await.unwrap();
    let base = QuizRaw {
        id: SHIPPED_QUIZ_ID.into(),
        difficulty: Difficulty::Easy,
        kind: None,
        question: "Q?".into(),
        answer: "A".into(),
        rubric: None,
    };
    overlay::amend_quiz(
        &conn,
        WITH_LESSON_ATOM,
        &base,
        Some(Difficulty::Hard),
        Some("new question".into()),
        Some("new answer".into()),
        None,
        None,
    )
    .await
    .expect("amend");

    let ov = overlay::load(&conn).await.expect("load");
    let entry = ov.atoms.get(WITH_LESSON_ATOM).expect("atom present");
    assert!(
        entry.removed.is_empty(),
        "amend must un-tombstone: {:?}",
        entry.removed
    );
    let stored = entry
        .quizzes
        .iter()
        .find(|q| q.id == SHIPPED_QUIZ_ID)
        .expect("amended quiz present");
    assert_eq!(stored.difficulty, Difficulty::Hard);
    assert_eq!(stored.question, "new question");
    assert_eq!(stored.answer, "new answer");
}

#[tokio::test]
async fn remove_quiz_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;

    overlay::remove_quiz(&conn, SHIPPED_QUIZ_ID).await.unwrap();
    overlay::remove_quiz(&conn, SHIPPED_QUIZ_ID).await.unwrap();

    // The tombstone table should still have exactly one row for this quiz.
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM overlay_removed_quizzes WHERE quiz_id = ?",
            params![SHIPPED_QUIZ_ID],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<i64>(0).unwrap(), 1);
}

// ── Graph::load_for_path merge semantics ──────────────────────────

#[tokio::test]
async fn merged_graph_fills_in_missing_lesson() {
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;

    let g = Graph::load_for_path(&conn, None).await.unwrap();
    assert!(
        g.by_id.get(NO_LESSON_ATOM).unwrap().lesson.is_none(),
        "atom must ship without a lesson"
    );

    overlay::add_lesson(&conn, NO_LESSON_ATOM, "Negation flips truth.")
        .await
        .unwrap();
    let g = Graph::load_for_path(&conn, None).await.unwrap();
    assert_eq!(
        g.by_id.get(NO_LESSON_ATOM).unwrap().lesson.as_deref(),
        Some("Negation flips truth."),
        "overlay lesson must appear in the merged view",
    );
}

#[tokio::test]
async fn merged_graph_appends_overlay_quizzes() {
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;

    let g = Graph::load_for_path(&conn, None).await.unwrap();
    let before = g.by_id.get(WITH_LESSON_ATOM).unwrap().quizzes.len();

    overlay::add_quiz(
        &conn,
        WITH_LESSON_ATOM,
        &QuizRaw {
            id: "fnd.1.1.1.q2".into(),
            difficulty: Difficulty::Hard,
            kind: None,
            question: "Restate the law of excluded middle.".into(),
            answer: "P ∨ ¬P".into(),
            rubric: None,
        },
    )
    .await
    .unwrap();

    let g = Graph::load_for_path(&conn, None).await.unwrap();
    let after = &g.by_id.get(WITH_LESSON_ATOM).unwrap().quizzes;
    assert_eq!(after.len(), before + 1);
    assert!(after.iter().any(|q| q.id == "fnd.1.1.1.q2"));
}

#[tokio::test]
async fn merged_graph_amendment_replaces_shipped_quiz() {
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;

    let base = QuizRaw {
        id: SHIPPED_QUIZ_ID.into(),
        difficulty: Difficulty::Easy,
        kind: None,
        question: "Q?".into(),
        answer: "A".into(),
        rubric: None,
    };
    overlay::amend_quiz(
        &conn,
        WITH_LESSON_ATOM,
        &base,
        None,
        Some("Define proposition.".into()),
        Some("Truth-valued declarative.".into()),
        None,
        None,
    )
    .await
    .unwrap();

    let g = Graph::load_for_path(&conn, None).await.unwrap();
    let q = g
        .by_id
        .get(WITH_LESSON_ATOM)
        .unwrap()
        .quizzes
        .iter()
        .find(|q| q.id == SHIPPED_QUIZ_ID)
        .expect("shipped quiz still present");
    assert_eq!(q.question, "Define proposition.");
    assert_eq!(q.answer, "Truth-valued declarative.");
}

#[tokio::test]
async fn merged_graph_drops_tombstoned_quizzes() {
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;

    overlay::remove_quiz(&conn, SHIPPED_QUIZ_ID).await.unwrap();

    let g = Graph::load_for_path(&conn, None).await.unwrap();
    let q = g
        .by_id
        .get(WITH_LESSON_ATOM)
        .unwrap()
        .quizzes
        .iter()
        .find(|q| q.id == SHIPPED_QUIZ_ID);
    assert!(q.is_none(), "tombstoned shipped quiz must not be merged");
}

// ── Global scope: writes are visible across all paths ──────────────

#[tokio::test]
async fn overlay_is_global_across_paths() {
    // Two paths on the same database: an overlay lesson authored
    // anywhere must appear in `load_for_path` regardless of which
    // path the loader is configured for.
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;
    for id in ["p_a", "p_b"] {
        conn.execute(
            "INSERT INTO paths(id, goal, created_at) VALUES (?, ?, ?)",
            params![id, "g", "2026-05-26T00:00:00Z"],
        )
        .await
        .unwrap();
    }

    overlay::add_lesson(&conn, NO_LESSON_ATOM, "shared lesson")
        .await
        .unwrap();

    // `load_for_path` doesn't take a path id anymore, but the
    // important invariant is that both paths see the same merged
    // graph. A single load reflects the global write.
    let g = Graph::load_for_path(&conn, None).await.unwrap();
    assert_eq!(
        g.by_id.get(NO_LESSON_ATOM).unwrap().lesson.as_deref(),
        Some("shared lesson"),
    );
}

// ── Graph validation helpers ──────────────────────────────────────

#[tokio::test]
async fn graph_atom_helper_rejects_clusters_and_unknowns() {
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;
    let g = Graph::load_for_path(&conn, None).await.unwrap();

    assert!(g.atom(WITH_LESSON_ATOM).is_ok(), "real atom resolves");
    assert!(
        matches!(g.atom("fnd.1"), Err(mathtutor::Error::NotAtom(_))),
        "cluster must fail with NotAtom",
    );
    assert!(
        matches!(
            g.atom("nope.does.not.exist"),
            Err(mathtutor::Error::AtomNotFound(_))
        ),
        "unknown id must fail with AtomNotFound",
    );
}

#[tokio::test]
async fn graph_quiz_helper_resolves_overlay_authored_ids() {
    // The validation must run against the *merged* graph, so a
    // freshly authored overlay quiz id must be recognized even though
    // the shipped curriculum doesn't know about it.
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;

    overlay::add_quiz(
        &conn,
        WITH_LESSON_ATOM,
        &QuizRaw {
            id: "fnd.1.1.1.q9".into(),
            difficulty: Difficulty::Hard,
            kind: None,
            question: "new".into(),
            answer: "new".into(),
            rubric: None,
        },
    )
    .await
    .unwrap();

    let g = Graph::load_for_path(&conn, None).await.unwrap();
    let (atom, quiz) = g.quiz("fnd.1.1.1.q9").expect("overlay quiz resolves");
    assert_eq!(atom.id, WITH_LESSON_ATOM);
    assert_eq!(quiz.id, "fnd.1.1.1.q9");
}
