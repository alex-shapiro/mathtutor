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
/// A body equal to the stored lesson changes nothing, so the command is safe to repeat.
pub async fn cmd_lesson_upsert(
    db: &Db,
    atom_id: &str,
    body: String,
    path_id: Option<&str>,
) -> Result<()> {
    db.transact(&Want::path(path_id), |view, changes| {
        let id = view.path()?.file.id.clone();
        let g = view.graph()?;
        let stored = g.atom(atom_id)?.lesson.as_deref();
        if stored == Some(body.as_str()) {
            return Ok(());
        }
        let amended = stored.is_some();

        overlay::upsert_lesson(changes, atom_id, &body);
        let atom = atom_id.to_string();
        let change = if amended {
            EventKind::LessonAmended { atom: atom.clone() }
        } else {
            EventKind::LessonAuthored { atom: atom.clone() }
        };
        changes.append(view, &Event::now(&id, change))?;
        changes.append(view, &Event::now(id, EventKind::LessonTaught { atom }))
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
        let tombstones = view.overlay.atoms.get(atom_id).map(|e| &e.removed);
        let quiz = draft
            .clone()
            .into_quiz(next_quiz_id(atom_id, &c.quizzes, tombstones));
        overlay::add_quiz(changes, atom_id, &quiz);
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
        overlay::amend_quiz(changes, &atom.id, quiz, patch);
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
            .overlay
            .atoms
            .values()
            .any(|a| a.removed.contains(quiz_id))
        {
            return Ok(());
        }
        let g = view.graph()?;
        let (atom, _) = g.quiz(quiz_id)?;
        overlay::remove_quiz(changes, &atom.id, quiz_id);
        let removed = EventKind::QuizRemoved {
            atom: atom.id.clone(),
            quiz: quiz_id.to_string(),
        };
        changes.append(view, &Event::now(id, removed))
    })
    .await
}

// Include tombstoned quiz IDs in the suffix scan so that deleted IDs are never reused.
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
