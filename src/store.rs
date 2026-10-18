//! Persist agent-authored content to the user overlay.
//!
//! `mt lesson upsert`, `mt quiz create`, `mt quiz update`, and `mt quiz
//! delete` write to the overlay tables — never to the shipped
//! curriculum. The shipped graph is read-only; user-authored content
//! lives globally on the user database and is shared across every path.
//!
//! Pre-condition checks consult the merged (shipped + overlay) graph:
//! a lesson is "missing" only if neither shipped nor overlay has one;
//! quiz IDs continue past the highest ID across both sources.

use std::collections::BTreeSet;
use std::path::Path;

use crate::db::Db;
use crate::event_log;
use crate::graph;
use crate::overlay;
use crate::types::{Difficulty, QuizType};
use crate::view::Want;
use crate::{Error, Result};

/// Upsert a lesson body for `atom_id` into the user overlay. Emits
/// `lesson_amended` if a lesson already existed in the merged view
/// (shipped or overlay), else `lesson_authored`. Always emits
/// `lesson_taught`: per the agent playbook, storing implies presenting.
pub async fn cmd_lesson_upsert(
    db: &Db,
    atom_id: &str,
    body: String,
    path_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<()> {
    db.transact(&Want::path(path_id), |view, changes| {
        let id = view.path()?.file.id.clone();
        let g = view.graph(graph_dir)?;
        let amended = g.atom(atom_id)?.lesson.is_some();

        overlay::upsert_lesson(changes, atom_id, &body);
        let change = if amended {
            event_log::lesson_amended(id.clone(), atom_id.to_string())
        } else {
            event_log::lesson_authored(id.clone(), atom_id.to_string())
        };
        changes.append(view, &change)?;
        changes.append(view, &event_log::lesson_taught(id, atom_id.to_string()))
    })
    .await
}

/// Persist a quiz on `atom_id` into the user overlay and log
/// `quiz_authored` against the active path. The new quiz ID continues
/// the `<atom>.qN` sequence past the highest existing N across shipped
/// + overlay so IDs are globally unique within the merged graph.
#[allow(clippy::too_many_arguments)]
pub async fn cmd_quiz_create(
    db: &Db,
    atom_id: &str,
    difficulty: Difficulty,
    question: String,
    answer: String,
    rubric: Option<String>,
    quiz_type: QuizType,
    path_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<String> {
    db.transact(&Want::path(path_id), |view, changes| {
        let id = view.path()?.file.id.clone();
        let g = view.graph(graph_dir)?;
        let c = g.atom(atom_id)?;
        if c.lesson.is_none() {
            return Err(Error::NoLesson(atom_id.to_string()));
        }
        let tombstones = view.overlay.atoms.get(atom_id).map(|e| &e.removed);
        let quiz = graph::Quiz {
            id: next_quiz_id(atom_id, &c.quizzes, tombstones),
            difficulty,
            kind: (quiz_type != QuizType::FreeText).then_some(quiz_type),
            question: question.clone(),
            answer: answer.clone(),
            rubric: rubric.clone(),
        };
        overlay::add_quiz(changes, atom_id, &quiz);
        changes.append(
            view,
            &event_log::quiz_authored(id, atom_id.to_string(), quiz.id.clone()),
        )?;
        Ok(quiz.id)
    })
    .await
}

/// Apply field-level edits to an existing quiz, writing the result
/// into the user overlay. The quiz may live in the shipped curriculum
/// or in the overlay; either way the post-amend state shadows or
/// replaces it in the merged graph.
///
/// FSRS history is preserved: the quiz id doesn't change, so the
/// scheduler keeps treating it as the same card. If you want a fresh
/// schedule, use `mt quiz delete` followed by `mt quiz create`.
#[allow(clippy::too_many_arguments)]
pub async fn cmd_quiz_update(
    db: &Db,
    quiz_id: &str,
    question: Option<String>,
    answer: Option<String>,
    rubric: Option<String>,
    difficulty: Option<Difficulty>,
    quiz_type: Option<QuizType>,
    path_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<()> {
    db.transact(&Want::path(path_id), |view, changes| {
        let id = view.path()?.file.id.clone();
        let g = view.graph(graph_dir)?;
        let (atom, quiz) = g.quiz(quiz_id)?;
        overlay::amend_quiz(
            changes,
            &atom.id,
            quiz,
            difficulty,
            question.as_deref(),
            answer.as_deref(),
            rubric.as_deref(),
            quiz_type,
        );
        changes.append(
            view,
            &event_log::quiz_amended(id, atom.id.clone(), quiz_id.to_string()),
        )
    })
    .await
}

/// Tombstone a quiz so it no longer appears in the merged view. Past
/// `QuizAnswered` events stay in the log for audit; the scheduler
/// simply stops surfacing it. Idempotent.
pub async fn cmd_quiz_delete(
    db: &Db,
    quiz_id: &str,
    path_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<()> {
    db.transact(&Want::path(path_id), |view, changes| {
        let id = view.path()?.file.id.clone();
        // Refuse to tombstone a name that never resolved to anything
        // (likely a typo).
        let g = view.graph(graph_dir)?;
        let (atom, _) = g.quiz(quiz_id)?;
        overlay::remove_quiz(changes, &atom.id, quiz_id);
        changes.append(
            view,
            &event_log::quiz_removed(id, atom.id.clone(), quiz_id.to_string()),
        )
    })
    .await
}

/// Returns `<atom>.q<n>` where `n` is one greater than the highest
/// `n` ever assigned for this atom — across live merged quizzes and
/// tombstones. Tombstoned ids are never recycled: the `overlay_quizzes`
/// row survives a delete, and reusing the id would both collide with
/// that row and conflate FSRS history across two logically distinct
/// quizzes.
fn next_quiz_id(
    atom_id: &str,
    quizzes: &[graph::Quiz],
    tombstones: Option<&BTreeSet<String>>,
) -> String {
    let prefix = format!("{atom_id}.q");
    let parse_n = |id: &str| id.strip_prefix(&prefix).and_then(|s| s.parse::<u32>().ok());
    let live = quizzes.iter().filter_map(|q| parse_n(&q.id));
    let dead = tombstones
        .into_iter()
        .flatten()
        .filter_map(|id| parse_n(id));
    let max = live.chain(dead).max().unwrap_or(0);
    format!("{prefix}{}", max + 1)
}
