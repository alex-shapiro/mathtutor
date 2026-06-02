//! Per-path progress snapshot consumed by the scheduler and the state
//! summaries.
//!
//! `PathProgress` answers the two questions the scheduler asks of the
//! event log without holding the log itself:
//!
//! - Which atoms have had their lesson taught (or authored) in this path?
//! - Which quizzes have ever been answered correctly?
//!
//! The struct can be built directly from a `Vec<Event>` (used by call
//! sites that already have the log loaded for other reasons — `mt path
//! next`'s envelope history, `mt path tree`'s rendering) or loaded over
//! SQL from the `events` and `cards` tables (used by `mt path state` and
//! anywhere else that has no need for the raw log).
//!
//! The cards-derived predicate `reps > lapses` is equivalent to "at least
//! one non-`Again` answer," because `lapses` only increments on `Again`
//! while `reps` increments on every answer (see `cards::apply_answer_to_cache`).

use std::collections::HashSet;

use libsql::Connection;

use crate::Result;
use crate::event_log::{Event, EventKind};

/// Cheap per-path snapshot: atoms taught, quizzes answered correctly.
#[derive(Debug, Default, Clone)]
pub struct PathProgress {
    pub taught_atoms: HashSet<String>,
    pub correct_quizzes: HashSet<String>,
}

impl PathProgress {
    pub fn lesson_taught(&self, atom_id: &str) -> bool {
        self.taught_atoms.contains(atom_id)
    }

    pub fn quiz_answered_correctly(&self, quiz_id: &str) -> bool {
        self.correct_quizzes.contains(quiz_id)
    }

    /// Build from an in-memory event log. Used by code paths that load
    /// the log for other reasons (envelope history, tree rendering).
    pub fn from_events(events: &[Event]) -> Self {
        let mut taught = HashSet::new();
        let mut correct = HashSet::new();
        for e in events {
            match e.kind {
                EventKind::LessonTaught | EventKind::LessonAuthored => {
                    if let Some(atom) = &e.atom {
                        taught.insert(atom.clone());
                    }
                }
                EventKind::QuizAnswered => {
                    if let (Some(q), Some(r)) = (&e.quiz, e.payload.rating)
                        && r.is_correct()
                    {
                        correct.insert(q.clone());
                    }
                }
                _ => {}
            }
        }
        Self {
            taught_atoms: taught,
            correct_quizzes: correct,
        }
    }

    /// Load over SQL — two indexed queries against `events` (lessons)
    /// and `cards` (correct quizzes). Avoids materializing the full
    /// event log, including the payload column.
    pub async fn load(conn: &Connection, path_id: &str) -> Result<Self> {
        let taught_atoms = load_taught_atoms(conn, path_id).await?;
        let correct_quizzes = load_correct_quizzes(conn, path_id).await?;
        Ok(Self {
            taught_atoms,
            correct_quizzes,
        })
    }
}

async fn load_taught_atoms(conn: &Connection, path_id: &str) -> Result<HashSet<String>> {
    let mut rows = conn
        .query(
            "SELECT DISTINCT atom_id FROM events \
             WHERE path_id = ? AND kind IN ('lesson_taught','lesson_authored') \
               AND atom_id IS NOT NULL",
            libsql::params![path_id],
        )
        .await?;
    let mut out = HashSet::new();
    while let Some(row) = rows.next().await? {
        let atom: String = row.get(0)?;
        out.insert(atom);
    }
    Ok(out)
}

async fn load_correct_quizzes(conn: &Connection, path_id: &str) -> Result<HashSet<String>> {
    // `reps > lapses` ⇔ at least one non-`Again` answer (lapses only
    // increments on `Again`). The cards table is the FSRS write-through
    // cache populated by `cards::apply_answer_to_cache` and rebuilt
    // deterministically by `cards::recompute`, so this matches the
    // event-log-derived `quiz_answered_correctly` exactly.
    let mut rows = conn
        .query(
            "SELECT quiz_id FROM cards WHERE path_id = ? AND reps > lapses",
            libsql::params![path_id],
        )
        .await?;
    let mut out = HashSet::new();
    while let Some(row) = rows.next().await? {
        let quiz: String = row.get(0)?;
        out.insert(quiz);
    }
    Ok(out)
}
