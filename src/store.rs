//! Persist agent-authored content to the active path's overlay.
//!
//! Both `mt store lesson` and `mt store quiz` write to
//! `~/.mathtutor/paths/<id>/overlay.ayml` — never to the shipped
//! curriculum. The shipped graph is read-only in V2; user-authored
//! content lives per-path so an unaudited lesson doesn't bleed into
//! sibling paths that target the same atom.
//!
//! Both commands consult the *merged* (shipped + overlay) graph for
//! pre-condition checks: a lesson is "missing" only if neither shipped
//! nor overlay has one; quiz IDs continue past the highest ID across
//! both sources.

use std::path::Path;

use libsql::Connection;

use crate::answer::atom_from_quiz_id;
use crate::event_log;
use crate::graph::{self, Graph, QuizRaw};
use crate::overlay;
use crate::path;
use crate::types::{Difficulty, QuizType};
use crate::{Error, Result};

/// Persist a lesson body for `atom_id` into the active path's overlay,
/// then log `lesson_authored` + `lesson_taught`. Per AGENTS.md the
/// agent presents the body to the user immediately after authoring,
/// so storing implies teaching.
pub async fn cmd_store_lesson(
    conn: &Connection,
    atom_id: &str,
    body: String,
    path_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<()> {
    let id = path::resolve_id(conn, path_id).await?;
    let g = Graph::load_for_path(&id, graph_dir)?;
    let c = g
        .by_id
        .get(atom_id)
        .ok_or_else(|| Error::AtomNotFound(atom_id.to_string()))?;
    if !c.children_ids.is_empty() {
        return Err(Error::NotAtom(atom_id.to_string()));
    }
    if c.lesson.is_some() {
        return Err(Error::LessonAlreadyExists(atom_id.to_string()));
    }

    overlay::add_lesson(&id, atom_id, body)?;
    let tx = conn.transaction().await?;
    event_log::append(
        &tx,
        &event_log::lesson_authored(id.clone(), atom_id.to_string()),
    )
    .await?;
    event_log::append(&tx, &event_log::lesson_taught(id, atom_id.to_string())).await?;
    tx.commit().await?;
    Ok(())
}

/// Persist a quiz on `atom_id` into the active path's overlay and log
/// `quiz_authored`. The new quiz ID continues the `<atom>.qN` sequence
/// past the highest existing N across shipped + overlay so IDs are
/// globally unique within the path's effective graph.
#[allow(clippy::too_many_arguments)]
pub async fn cmd_store_quiz(
    conn: &Connection,
    atom_id: &str,
    difficulty: Difficulty,
    question: String,
    answer: String,
    rubric: Option<String>,
    quiz_type: QuizType,
    path_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<String> {
    let id = path::resolve_id(conn, path_id).await?;
    let g = Graph::load_for_path(&id, graph_dir)?;
    let c = g
        .by_id
        .get(atom_id)
        .ok_or_else(|| Error::AtomNotFound(atom_id.to_string()))?;
    if !c.children_ids.is_empty() {
        return Err(Error::NotAtom(atom_id.to_string()));
    }
    if c.lesson.is_none() {
        return Err(Error::NoLesson(atom_id.to_string()));
    }

    let new_id = next_quiz_id(atom_id, &c.quizzes);
    overlay::add_quiz(
        &id,
        atom_id,
        new_id.clone(),
        difficulty,
        question,
        answer,
        rubric,
        quiz_type,
    )?;
    let tx = conn.transaction().await?;
    event_log::append(
        &tx,
        &event_log::quiz_authored(id, atom_id.to_string(), new_id.clone()),
    )
    .await?;
    tx.commit().await?;
    Ok(new_id)
}

/// Apply field-level edits to an existing quiz, writing the result into
/// the active path's overlay. The quiz may live in the shipped
/// curriculum or in the overlay; either way the post-amend state
/// shadows or replaces it during the next `Graph::load_for_path`.
///
/// FSRS history is preserved: the quiz id doesn't change, so the
/// scheduler keeps treating it as the same card. If you want a fresh
/// schedule, use `mt remove quiz` followed by `mt store quiz`.
#[allow(clippy::too_many_arguments)]
pub async fn cmd_amend_quiz(
    conn: &Connection,
    quiz_id: &str,
    question: Option<String>,
    answer: Option<String>,
    rubric: Option<String>,
    difficulty: Option<Difficulty>,
    quiz_type: Option<QuizType>,
    path_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<()> {
    let atom_id =
        atom_from_quiz_id(quiz_id).ok_or_else(|| Error::UnknownId(quiz_id.to_string()))?;
    let id = path::resolve_id(conn, path_id).await?;
    let g = Graph::load_for_path(&id, graph_dir)?;
    let c = g
        .by_id
        .get(&atom_id)
        .ok_or_else(|| Error::AtomNotFound(atom_id.clone()))?;
    let base = c
        .quizzes
        .iter()
        .find(|q| q.id == quiz_id)
        .ok_or_else(|| Error::UnknownId(quiz_id.to_string()))?;
    let base_raw = QuizRaw {
        id: base.id.clone(),
        difficulty: base.difficulty,
        kind: base.kind,
        question: base.question.clone(),
        answer: base.answer.clone(),
        rubric: base.rubric.clone(),
    };
    overlay::amend_quiz(
        &id, &atom_id, &base_raw, difficulty, question, answer, rubric, quiz_type,
    )?;
    let tx = conn.transaction().await?;
    event_log::append(
        &tx,
        &event_log::quiz_amended(id, atom_id, quiz_id.to_string()),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Tombstone a quiz so it no longer appears in the merged view for
/// this path. Past `QuizAnswered` events stay in the log for audit;
/// the scheduler simply stops surfacing it.
pub async fn cmd_remove_quiz(
    conn: &Connection,
    quiz_id: &str,
    path_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<()> {
    let atom_id =
        atom_from_quiz_id(quiz_id).ok_or_else(|| Error::UnknownId(quiz_id.to_string()))?;
    let id = path::resolve_id(conn, path_id).await?;

    // Confirm the quiz exists in the merged view; refuse to tombstone
    // a name that never resolved to anything (likely a typo).
    let g = Graph::load_for_path(&id, graph_dir)?;
    let c = g
        .by_id
        .get(&atom_id)
        .ok_or_else(|| Error::AtomNotFound(atom_id.clone()))?;
    if !c.quizzes.iter().any(|q| q.id == quiz_id) {
        return Err(Error::UnknownId(quiz_id.to_string()));
    }

    overlay::remove_quiz(&id, &atom_id, quiz_id)?;
    let tx = conn.transaction().await?;
    event_log::append(
        &tx,
        &event_log::quiz_removed(id, atom_id, quiz_id.to_string()),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Returns `<atom>.q<n>` where `n` is one greater than the highest
/// existing n in the merged view. Stable: gaps from deletions are
/// never reused.
fn next_quiz_id(atom_id: &str, quizzes: &[graph::Quiz]) -> String {
    let prefix = format!("{atom_id}.q");
    let max = quizzes
        .iter()
        .filter_map(|q| q.id.strip_prefix(&prefix))
        .filter_map(|s| s.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    format!("{prefix}{}", max + 1)
}
