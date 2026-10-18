//! Integration tests for the table-backed user overlay.
//!
//! Each test uses a fresh Baserunna database and drives `overlay::*`
//! plus `Graph::merged` to verify three contracts:
//!
//! 1. CRUD operations write the expected rows to the overlay tables.
//! 2. `Graph::merged` merges the user overlay on top of the
//!    shipped curriculum (lessons, amendments, tombstones, new quizzes).
//! 3. Overlays are global — a write made under one path is visible to
//!    every other path on the same database.

use mathtutor::db::Db;
use mathtutor::graph::{Graph, Quiz};
use mathtutor::overlay;
use mathtutor::types::{Difficulty, QuizType};
use mathtutor::view::Changes;

mod common;

use common::PATH_ID;

/// Atom that ships *without* a lesson in `curriculum/graph` — used to
/// verify `upsert_lesson` writes new content rather than overwrite the
/// shipped data.
const NO_LESSON_ATOM: &str = "fnd.1.1.2";
/// Atom that ships *with* a lesson and one quiz (`fnd.1.1.1.q1`) —
/// used to verify merge semantics for existing content.
const WITH_LESSON_ATOM: &str = "fnd.1.1.1";
const SHIPPED_QUIZ_ID: &str = "fnd.1.1.1.q1";

async fn fresh_db() -> Db {
    common::fresh_db(PATH_ID).await
}

/// Commit the overlay changes `f` makes.
async fn edit(db: &Db, f: impl Fn(&mut Changes)) {
    common::write(db, PATH_ID, |_, changes| f(changes)).await;
}

/// The merged graph as stored now.
async fn merged(db: &Db) -> Graph {
    Graph::merged(&common::overlay(db).await, None).unwrap()
}

fn quiz(
    id: &str,
    difficulty: Difficulty,
    kind: Option<QuizType>,
    question: &str,
    answer: &str,
    rubric: Option<&str>,
) -> Quiz {
    Quiz {
        id: id.into(),
        difficulty,
        kind,
        question: question.into(),
        answer: answer.into(),
        rubric: rubric.map(str::to_string),
    }
}

#[tokio::test]
async fn load_returns_empty_overlay_on_fresh_db() {
    let db = fresh_db().await;
    let ov = common::overlay(&db).await;
    assert!(ov.atoms.is_empty());
}

#[tokio::test]
async fn upsert_lesson_writes_row_and_round_trips() {
    let db = fresh_db().await;

    edit(&db, |c| {
        overlay::upsert_lesson(c, NO_LESSON_ATOM, "Negation flips truth.");
    })
    .await;

    let ov = common::overlay(&db).await;
    let entry = ov.atoms.get(NO_LESSON_ATOM).expect("atom present");
    assert_eq!(entry.lesson.as_deref(), Some("Negation flips truth."));
}

#[tokio::test]
async fn upsert_lesson_overwrites_existing_overlay_row() {
    // A second upsert for the same atom replaces the body instead of
    // raising a unique-violation.
    let db = fresh_db().await;

    edit(&db, |c| overlay::upsert_lesson(c, NO_LESSON_ATOM, "first")).await;
    edit(&db, |c| overlay::upsert_lesson(c, NO_LESSON_ATOM, "second")).await;

    let ov = common::overlay(&db).await;
    let entry = ov.atoms.get(NO_LESSON_ATOM).expect("atom present");
    assert_eq!(entry.lesson.as_deref(), Some("second"));
}

#[tokio::test]
async fn add_quiz_writes_row_and_round_trips() {
    let db = fresh_db().await;

    edit(&db, |c| {
        overlay::add_quiz(
            c,
            NO_LESSON_ATOM,
            &quiz(
                "fnd.1.1.2.q1",
                Difficulty::Medium,
                Some(QuizType::MultipleChoice),
                "Pick the negation",
                "¬P",
                Some("Look for the ¬ glyph"),
            ),
        );
    })
    .await;

    let ov = common::overlay(&db).await;
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
    let db = fresh_db().await;

    // Tombstone the shipped quiz, then amend it. The amend must
    // un-tombstone so the quiz comes back into the merged view.
    edit(&db, |c| {
        overlay::remove_quiz(c, WITH_LESSON_ATOM, SHIPPED_QUIZ_ID);
    })
    .await;
    let base = Quiz {
        id: SHIPPED_QUIZ_ID.into(),
        difficulty: Difficulty::Easy,
        kind: None,
        question: "Q?".into(),
        answer: "A".into(),
        rubric: None,
    };
    edit(&db, |c| {
        overlay::amend_quiz(
            c,
            WITH_LESSON_ATOM,
            &base,
            Some(Difficulty::Hard),
            Some("new question"),
            Some("new answer"),
            None,
            None,
        );
    })
    .await;

    let ov = common::overlay(&db).await;
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
    let db = fresh_db().await;

    edit(&db, |c| {
        overlay::remove_quiz(c, WITH_LESSON_ATOM, SHIPPED_QUIZ_ID);
    })
    .await;
    edit(&db, |c| {
        overlay::remove_quiz(c, WITH_LESSON_ATOM, SHIPPED_QUIZ_ID);
    })
    .await;

    // Exactly one tombstone for this quiz.
    let ov = common::overlay(&db).await;
    let removed = &ov
        .atoms
        .get(WITH_LESSON_ATOM)
        .expect("atom present")
        .removed;
    assert_eq!(removed.iter().collect::<Vec<_>>(), [SHIPPED_QUIZ_ID]);
}

// ── Graph::merged merge semantics ──────────────────────────

#[tokio::test]
async fn merged_graph_fills_in_missing_lesson() {
    let db = fresh_db().await;

    let g = merged(&db).await;
    assert!(
        g.by_id.get(NO_LESSON_ATOM).unwrap().lesson.is_none(),
        "atom must ship without a lesson"
    );

    edit(&db, |c| {
        overlay::upsert_lesson(c, NO_LESSON_ATOM, "Negation flips truth.");
    })
    .await;
    let g = merged(&db).await;
    assert_eq!(
        g.by_id.get(NO_LESSON_ATOM).unwrap().lesson.as_deref(),
        Some("Negation flips truth."),
        "overlay lesson must appear in the merged view",
    );
}

#[tokio::test]
async fn merged_graph_overlay_lesson_overrides_shipped() {
    // The conflict-resolution contract on `Graph::merged`: an
    // overlay lesson with the same atom_id wins over the shipped one.
    let db = fresh_db().await;

    let shipped = merged(&db)
        .await
        .by_id
        .get(WITH_LESSON_ATOM)
        .unwrap()
        .lesson
        .clone();
    assert!(shipped.is_some(), "atom must ship with a lesson");

    edit(&db, |c| {
        overlay::upsert_lesson(c, WITH_LESSON_ATOM, "overlay body");
    })
    .await;
    let g = merged(&db).await;
    assert_eq!(
        g.by_id.get(WITH_LESSON_ATOM).unwrap().lesson.as_deref(),
        Some("overlay body"),
        "overlay lesson must override the shipped lesson",
    );
}

#[tokio::test]
async fn merged_graph_appends_overlay_quizzes() {
    let db = fresh_db().await;

    let g = merged(&db).await;
    let before = g.by_id.get(WITH_LESSON_ATOM).unwrap().quizzes.len();

    edit(&db, |c| {
        overlay::add_quiz(
            c,
            WITH_LESSON_ATOM,
            &quiz(
                "fnd.1.1.1.q2",
                Difficulty::Hard,
                None,
                "Restate the law of excluded middle.",
                "P ∨ ¬P",
                None,
            ),
        );
    })
    .await;

    let g = merged(&db).await;
    let after = &g.by_id.get(WITH_LESSON_ATOM).unwrap().quizzes;
    assert_eq!(after.len(), before + 1);
    assert!(after.iter().any(|q| q.id == "fnd.1.1.1.q2"));
}

#[tokio::test]
async fn merged_graph_amendment_replaces_shipped_quiz() {
    let db = fresh_db().await;

    let base = Quiz {
        id: SHIPPED_QUIZ_ID.into(),
        difficulty: Difficulty::Easy,
        kind: None,
        question: "Q?".into(),
        answer: "A".into(),
        rubric: None,
    };
    edit(&db, |c| {
        overlay::amend_quiz(
            c,
            WITH_LESSON_ATOM,
            &base,
            None,
            Some("Define proposition."),
            Some("Truth-valued declarative."),
            None,
            None,
        );
    })
    .await;

    let g = merged(&db).await;
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
    let db = fresh_db().await;

    edit(&db, |c| {
        overlay::remove_quiz(c, WITH_LESSON_ATOM, SHIPPED_QUIZ_ID);
    })
    .await;

    let g = merged(&db).await;
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
    // anywhere must appear in `merged` regardless of which
    // path the loader is configured for.
    let db = fresh_db().await;
    for id in ["p_a", "p_b"] {
        common::seed_path(&db, id, &[], mathtutor::types::Strategy::BottomUp).await;
    }

    edit(&db, |c| {
        overlay::upsert_lesson(c, NO_LESSON_ATOM, "shared lesson");
    })
    .await;

    // Both paths' views carry the same merged overlay.
    for id in ["p_a", "p_b"] {
        let view = db
            .snapshot(&mathtutor::view::Want::path(Some(id)))
            .await
            .unwrap();
        assert_eq!(view.path().unwrap().file.id, id);
        let g = Graph::merged(&view.overlay, None).unwrap();
        assert_eq!(
            g.by_id.get(NO_LESSON_ATOM).unwrap().lesson.as_deref(),
            Some("shared lesson"),
        );
    }
}

// ── Graph validation helpers ──────────────────────────────────────

#[tokio::test]
async fn graph_atom_helper_rejects_clusters_and_unknowns() {
    let db = fresh_db().await;
    let g = merged(&db).await;

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
    let db = fresh_db().await;

    edit(&db, |c| {
        overlay::add_quiz(
            c,
            WITH_LESSON_ATOM,
            &quiz("fnd.1.1.1.q9", Difficulty::Hard, None, "new", "new", None),
        );
    })
    .await;

    let g = merged(&db).await;
    let (atom, quiz) = g.quiz("fnd.1.1.1.q9").expect("overlay quiz resolves");
    assert_eq!(atom.id, WITH_LESSON_ATOM);
    assert_eq!(quiz.id, "fnd.1.1.1.q9");
}
