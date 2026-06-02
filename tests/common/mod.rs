//! Shared helpers for integration tests.
//!
//! `progress_of` mirrors the SQL projection that `PathProgress::load`
//! runs in production, but reads a synthetic in-memory event list.
//! Tests assert scheduler / state behavior by constructing events and
//! folding them through the same predicates the production loader uses.

use mathtutor::event_log::{Event, EventKind};
use mathtutor::progress::PathProgress;

#[allow(dead_code)] // referenced by some test files but not all.
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
