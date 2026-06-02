//! Per-path progress snapshot consumed by the scheduler and the state
//! summaries.
//!
//! `PathProgress` answers the two questions the scheduler asks of the
//! event log without holding the log itself:
//!
//! - Which atoms have had their lesson taught or authored in this path?
//! - Which quizzes have ever been answered correctly?
//!
//! Loaded from SQL projections to avoid materializing the event log.

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

    /// Load over SQL — two indexed queries against `events` (lessons)
    /// and `cards` (correct quizzes). The production entry point.
    pub async fn load(conn: &Connection, path_id: &str) -> Result<Self> {
        let taught_atoms = load_taught_atoms(conn, path_id).await?;
        let correct_quizzes = load_correct_quizzes(conn, path_id).await?;
        Ok(Self {
            taught_atoms,
            correct_quizzes,
        })
    }

    /// Fold a synthetic event log into the same shape `load` produces.
    /// Test-only — production never has a `Vec<Event>` in hand to fold.
    pub fn from_events(events: &[Event]) -> Self {
        let mut p = Self::default();
        for e in events {
            match e.kind {
                EventKind::LessonTaught | EventKind::LessonAuthored => {
                    if let Some(atom) = &e.atom {
                        p.taught_atoms.insert(atom.clone());
                    }
                }
                EventKind::QuizAnswered => {
                    if let (Some(q), Some(r)) = (&e.quiz, e.payload.rating)
                        && r.is_correct()
                    {
                        p.correct_quizzes.insert(q.clone());
                    }
                }
                _ => {}
            }
        }
        p
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
    // `reps > lapses` ⇔ at least one non-`Again` answer, because
    // `lapses` only increments on `Again`.
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
