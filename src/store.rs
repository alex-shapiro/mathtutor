//! Commands that store user-authored lessons and quizzes in the curriculum overlay.

use std::collections::BTreeSet;

use crate::db::Db;
use crate::event_log::{Event, EventKind};
use crate::graph;
use crate::overlay::{self, QuizDraft, QuizPatch};
use crate::view::Want;
use crate::{Error, Result};

/// Store a lesson body for `atom_id` in the overlay and log its authoring and presentation events.
///
/// A body equal to the stored lesson records only the presentation, and only if the path has not taught the lesson, so the command is safe to repeat.
pub async fn cmd_lesson_upsert(
    db: &Db,
    atom_id: &str,
    body: String,
    path_id: Option<&str>,
) -> Result<()> {
    db.transact(&Want::path(path_id), |view, changes| {
        let ps = view.path()?;
        let id = ps.file.id.clone();
        let g = view.graph()?;
        let stored = g.atom(atom_id)?.lesson.as_deref();
        let atom = atom_id.to_string();
        let taught = Event::now(&id, EventKind::LessonTaught { atom: atom.clone() });
        if stored == Some(body.as_str()) {
            if ps.projection.taught.contains_key(atom_id) {
                return Ok(());
            }
            return changes.append(view, &taught);
        }

        overlay::upsert_lesson(changes, view, atom_id, &body)?;
        let change = if stored.is_some() {
            EventKind::LessonAmended { atom }
        } else {
            EventKind::LessonAuthored { atom }
        };
        changes.append(view, &Event::now(&id, change))?;
        changes.append(view, &taught)
    })
    .await
}

/// Create quiz `draft` for `atom_id` in the overlay and return its generated quiz ID.
pub async fn cmd_quiz_create(
    db: &Db,
    atom_id: &str,
    draft: QuizDraft,
    path_id: Option<&str>,
) -> Result<String> {
    db.transact(&Want::path(path_id), |view, changes| {
        let id = view.path()?.file.id.clone();
        let g = view.graph()?;
        let c = g.atom(atom_id)?;
        if c.lesson.is_none() {
            return Err(Error::NoLesson(atom_id.to_string()));
        }
        let tombstones = view.overlay()?.atoms.get(atom_id).map(|e| &e.removed);
        let quiz = draft
            .clone()
            .into_quiz(next_quiz_id(atom_id, &c.quizzes, tombstones));
        overlay::add_quiz(changes, view, atom_id, &quiz)?;
        let authored = EventKind::QuizAuthored {
            atom: atom_id.to_string(),
            quiz: quiz.id.clone(),
        };
        changes.append(view, &Event::now(id, authored))?;
        Ok(quiz.id)
    })
    .await
}

/// Apply `patch` to `quiz_id` in the overlay while preserving its ID and FSRS history.
pub async fn cmd_quiz_update(
    db: &Db,
    quiz_id: &str,
    patch: &QuizPatch,
    path_id: Option<&str>,
) -> Result<()> {
    db.transact(&Want::path(path_id), |view, changes| {
        let id = view.path()?.file.id.clone();
        let g = view.graph()?;
        let (atom, quiz) = g.quiz(quiz_id)?;
        overlay::amend_quiz(changes, view, &atom.id, quiz, patch)?;
        let amended = EventKind::QuizAmended {
            atom: atom.id.clone(),
            quiz: quiz_id.to_string(),
        };
        changes.append(view, &Event::now(id, amended))
    })
    .await
}

/// Mark `quiz_id` as removed in the overlay and log a `quiz_removed` event.
pub async fn cmd_quiz_delete(db: &Db, quiz_id: &str, path_id: Option<&str>) -> Result<()> {
    db.transact(&Want::path(path_id), |view, changes| {
        let id = view.path()?.file.id.clone();
        if view
            .overlay()?
            .atoms
            .values()
            .any(|a| a.removed.contains(quiz_id))
        {
            return Ok(());
        }
        let g = view.graph()?;
        let (atom, _) = g.quiz(quiz_id)?;
        overlay::remove_quiz(changes, view, &atom.id, quiz_id)?;
        let removed = EventKind::QuizRemoved {
            atom: atom.id.clone(),
            quiz: quiz_id.to_string(),
        };
        changes.append(view, &Event::now(id, removed))
    })
    .await
}

/// Return the next user quiz ID of `atom_id`, `<atom_id>.u<N>`, above every live and removed user quiz ID of the atom.
///
/// Curriculum quizzes use `<atom_id>.q<N>`, so a curriculum release cannot reuse the ID of a user quiz.
fn next_quiz_id(
    atom_id: &str,
    quizzes: &[graph::Quiz],
    tombstones: Option<&BTreeSet<String>>,
) -> String {
    let prefix = format!("{atom_id}.u");
    let parse_n = |id: &str| id.strip_prefix(&prefix).and_then(|s| s.parse::<u32>().ok());
    let live = quizzes.iter().filter_map(|q| parse_n(&q.id));
    let dead = tombstones
        .into_iter()
        .flatten()
        .filter_map(|id| parse_n(id));
    let max = live.chain(dead).max().unwrap_or(0);
    format!("{prefix}{}", max + 1)
}
