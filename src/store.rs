//! Commands that store user-authored lessons and quizzes in the curriculum overlay.

use std::collections::BTreeSet;
use std::path::Path;

use crate::db::Db;
use crate::event_log;
use crate::graph;
use crate::overlay;
use crate::types::{Difficulty, QuizType};
use crate::view::Want;
use crate::{Error, Result};

/// Store a lesson body for `atom_id` in the overlay and log its authoring and presentation events.
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

/// Create a quiz for `atom_id` in the overlay and return its generated quiz ID.
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

/// Update fields of `quiz_id` in the overlay while preserving its ID and FSRS history.
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

/// Mark `quiz_id` as removed in the overlay and log a `quiz_removed` event.
pub async fn cmd_quiz_delete(
    db: &Db,
    quiz_id: &str,
    path_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<()> {
    db.transact(&Want::path(path_id), |view, changes| {
        let id = view.path()?.file.id.clone();
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

// Include tombstoned quiz IDs when assigning the next suffix so deleted IDs are never reused.
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
