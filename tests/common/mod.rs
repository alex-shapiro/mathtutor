//! Shared helpers for integration tests.
//!
//! Each test binary in `tests/` compiles this module independently, so
//! helpers that one binary doesn't call are flagged dead by the linter.
//! Allow at the module level — every helper has at least one caller
//! somewhere in the integration suite.

#![allow(dead_code)]

use libsql::{Connection, params};
use mathtutor::db::{self, DbConfig};
use mathtutor::event_log::{Event, EventKind};
use mathtutor::progress::PathProgress;
use tempfile::TempDir;

/// Open a freshly-migrated libSQL database under `dir` and seed one
/// path row keyed by `path_id`. Callers add their own `path_targets`,
/// events, etc. on top.
pub async fn fresh_db(dir: &TempDir, path_id: &str) -> Connection {
    let cfg = DbConfig::local(dir.path().join("mt.db"));
    let database = db::open(&cfg).await.expect("open");
    let conn = db::connect(&database).await.expect("connect");
    conn.execute(
        "INSERT INTO paths(id, goal, created_at) VALUES (?, ?, ?)",
        params![path_id, "test goal", "2026-05-26T00:00:00Z"],
    )
    .await
    .expect("seed path");
    conn
}

/// Fold a synthetic event log into a `PathProgress`. Mirrors what the
/// production `PathProgress::load` projects out of `events` and `cards`,
/// so tests can pin walker behavior without standing up a database.
pub fn progress_of(events: &[Event]) -> PathProgress {
    let mut p = PathProgress::default();
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
