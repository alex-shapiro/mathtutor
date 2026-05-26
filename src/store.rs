//! Persist agent-authored content to the global overlay.
//!
//! `mt store lesson`, `mt store quiz`, `mt amend quiz`, and `mt remove
//! quiz` write to the SQL overlay tables — never to the shipped
//! curriculum. The shipped graph is read-only; user-authored content
//! lives globally on the user database and is shared across every path.
//!
//! Pre-condition checks consult the merged (shipped + overlay) graph:
//! a lesson is "missing" only if neither shipped nor overlay has one;
//! quiz IDs continue past the highest ID across both sources.

use std::path::Path;

use libsql::Connection;

use crate::event_log;
use crate::graph::{self, Graph};
use crate::overlay;
use crate::path;
use crate::types::{Difficulty, QuizType};
use crate::{Error, Result};

/// Upsert a lesson body for `atom_id` into the global overlay. Emits
/// `lesson_amended` if a lesson already existed in the merged view
/// (shipped or overlay), else `lesson_authored`. Always emits
/// `lesson_taught`: per the agent playbook, storing implies presenting.
pub async fn cmd_store_lesson(
    conn: &Connection,
    atom_id: &str,
    body: String,
    path_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<()> {
    let tx = conn.transaction().await?;
    let id = path::resolve_id(&tx, path_id).await?;
    let g = Graph::load_for_path(&tx, graph_dir).await?;
    let amended = g.atom(atom_id)?.lesson.is_some();

    overlay::upsert_lesson(&tx, atom_id, &body).await?;
    let change = if amended {
        event_log::lesson_amended(id.clone(), atom_id.to_string())
    } else {
        event_log::lesson_authored(id.clone(), atom_id.to_string())
    };
    event_log::append(&tx, &change).await?;
    event_log::append(&tx, &event_log::lesson_taught(id, atom_id.to_string())).await?;
    tx.commit().await?;
    Ok(())
}

/// Persist a quiz on `atom_id` into the global overlay and log
/// `quiz_authored` against the active path. The new quiz ID continues
/// the `<atom>.qN` sequence past the highest existing N across shipped
/// + overlay so IDs are globally unique within the merged graph.
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
    let tx = conn.transaction().await?;
    let id = path::resolve_id(&tx, path_id).await?;
    let g = Graph::load_for_path(&tx, graph_dir).await?;
    let c = g.atom(atom_id)?;
    if c.lesson.is_none() {
        return Err(Error::NoLesson(atom_id.to_string()));
    }

    let new_id = next_quiz_id(atom_id, &c.quizzes);
    let kind = (quiz_type != QuizType::FreeText).then_some(quiz_type);
    overlay::add_quiz(
        &tx,
        atom_id,
        &new_id,
        difficulty,
        kind,
        &question,
        &answer,
        rubric.as_deref(),
    )
    .await?;
    event_log::append(
        &tx,
        &event_log::quiz_authored(id, atom_id.to_string(), new_id.clone()),
    )
    .await?;
    tx.commit().await?;
    Ok(new_id)
}

/// Apply field-level edits to an existing quiz, writing the result
/// into the global overlay. The quiz may live in the shipped curriculum
/// or in the overlay; either way the post-amend state shadows or
/// replaces it during the next `Graph::load_for_path`.
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
    let tx = conn.transaction().await?;
    let id = path::resolve_id(&tx, path_id).await?;
    let g = Graph::load_for_path(&tx, graph_dir).await?;
    let (atom, quiz) = g.quiz(quiz_id)?;
    overlay::amend_quiz(
        &tx,
        &atom.id,
        quiz,
        difficulty,
        question.as_deref(),
        answer.as_deref(),
        rubric.as_deref(),
        quiz_type,
    )
    .await?;
    event_log::append(
        &tx,
        &event_log::quiz_amended(id, atom.id.clone(), quiz_id.to_string()),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Tombstone a quiz so it no longer appears in the merged view. Past
/// `QuizAnswered` events stay in the log for audit; the scheduler
/// simply stops surfacing it. Idempotent.
pub async fn cmd_remove_quiz(
    conn: &Connection,
    quiz_id: &str,
    path_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<()> {
    // Confirm the quiz exists in the merged view; refuse to tombstone
    // a name that never resolved to anything (likely a typo).
    let tx = conn.transaction().await?;
    let id = path::resolve_id(&tx, path_id).await?;
    let g = Graph::load_for_path(&tx, graph_dir).await?;
    let (atom, _) = g.quiz(quiz_id)?;

    overlay::remove_quiz(&tx, quiz_id).await?;
    event_log::append(
        &tx,
        &event_log::quiz_removed(id, atom.id.clone(), quiz_id.to_string()),
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
