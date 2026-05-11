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

use crate::event_log;
use crate::graph::{self, Graph};
use crate::overlay;
use crate::path;
use crate::types::{Difficulty, QuizType};
use crate::{Error, Result};

/// Persist a lesson body for `atom_id` into the active path's overlay,
/// then log `lesson_authored` + `lesson_taught`. Per AGENTS.md the
/// agent presents the body to the user immediately after authoring,
/// so storing implies teaching.
pub fn cmd_store_lesson(
    atom_id: &str,
    body: String,
    path_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<()> {
    let id = path::resolve_id(path_id)?;
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
    event_log::append(event_log::lesson_authored(id.clone(), atom_id.to_string()))?;
    event_log::append(event_log::lesson_taught(id, atom_id.to_string()))?;
    Ok(())
}

/// Persist a quiz on `atom_id` into the active path's overlay and log
/// `quiz_authored`. The new quiz ID continues the `<atom>.qN` sequence
/// past the highest existing N across shipped + overlay so IDs are
/// globally unique within the path's effective graph.
#[allow(clippy::too_many_arguments)]
pub fn cmd_store_quiz(
    atom_id: &str,
    difficulty: Difficulty,
    question: String,
    answer: String,
    rubric: Option<String>,
    quiz_type: QuizType,
    path_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<String> {
    let id = path::resolve_id(path_id)?;
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
    event_log::append(event_log::quiz_authored(
        id,
        atom_id.to_string(),
        new_id.clone(),
    ))?;
    Ok(new_id)
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
