//! Shared helpers for integration tests.
//!
//! Each test binary in `tests/` compiles this module independently and
//! flags different dead code. To avoid, we allow dead code here.

#![allow(dead_code)]

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use libsql::{Connection, params};
use mathtutor::db::{self, DbConfig};
use mathtutor::event_log::{Event, EventKind, EventPayload};
use mathtutor::graph::{FlatConcept, Graph, Quiz};
use mathtutor::path::PathFile;
use mathtutor::progress::PathProgress;
use mathtutor::types::{Difficulty, Rating, Strategy};
use tempfile::TempDir;

pub const PATH_ID: &str = "p_test";

// ── Database ────────────────────────────────────────────────────────

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

// ── Concept builders ────────────────────────────────────────────────

pub fn quiz(id: &str, difficulty: Difficulty) -> Quiz {
    Quiz {
        id: id.into(),
        difficulty,
        kind: None,
        question: "q".into(),
        answer: "a".into(),
        rubric: None,
    }
}

pub fn atom(id: &str, prereqs: &[&str], lesson: Option<&str>, quizzes: Vec<Quiz>) -> FlatConcept {
    FlatConcept {
        id: id.into(),
        name: id.into(),
        description: None,
        prerequisites: prereqs.iter().map(|s| (*s).to_string()).collect(),
        children_ids: Vec::new(),
        lesson: lesson.map(String::from),
        quizzes,
    }
}

pub fn cluster(id: &str, children: &[&str]) -> FlatConcept {
    FlatConcept {
        id: id.into(),
        name: id.into(),
        description: None,
        prerequisites: Vec::new(),
        children_ids: children.iter().map(|s| (*s).to_string()).collect(),
        lesson: None,
        quizzes: Vec::new(),
    }
}

/// `atom` with no lesson and no quizzes.
/// Represents a leaf that must be authored from scratch.
pub fn empty_atom(id: &str, prereqs: &[&str]) -> FlatConcept {
    atom(id, prereqs, None, Vec::new())
}

/// `atom` pre-populated with a lesson body and one quiz per difficulty,
/// IDs derived as `{id}.q1`/`.q2`/`.q3`.
pub fn complete_atom(id: &str, prereqs: &[&str]) -> FlatConcept {
    atom(
        id,
        prereqs,
        Some("body"),
        vec![
            quiz(&format!("{id}.q1"), Difficulty::Easy),
            quiz(&format!("{id}.q2"), Difficulty::Medium),
            quiz(&format!("{id}.q3"), Difficulty::Hard),
        ],
    )
}

pub fn graph_of(concepts: Vec<FlatConcept>) -> Graph {
    let mut by_id = HashMap::new();
    for c in concepts {
        by_id.insert(c.id.clone(), c);
    }
    Graph { by_id }
}

pub fn path_with(targets: &[&str]) -> PathFile {
    path_with_strategy(targets, Strategy::BottomUp)
}

pub fn path_with_strategy(targets: &[&str], strategy: Strategy) -> PathFile {
    PathFile {
        id: PATH_ID.into(),
        goal: "test".into(),
        created_at: Utc::now(),
        targets: targets.iter().map(|s| (*s).to_string()).collect(),
        strategy,
    }
}

// ── Event builders ──────────────────────────────────────────────────

pub fn taught(atom_id: &str) -> Event {
    taught_at(atom_id, Utc::now())
}

pub fn taught_at(atom_id: &str, ts: DateTime<Utc>) -> Event {
    Event {
        ts,
        kind: EventKind::LessonTaught,
        path: PATH_ID.into(),
        atom: Some(atom_id.into()),
        quiz: None,
        payload: EventPayload::default(),
    }
}

pub fn lesson_authored(atom_id: &str) -> Event {
    Event {
        ts: Utc::now(),
        kind: EventKind::LessonAuthored,
        path: PATH_ID.into(),
        atom: Some(atom_id.into()),
        quiz: None,
        payload: EventPayload::default(),
    }
}

pub fn answered(quiz_id: &str, rating: Rating) -> Event {
    answered_at(quiz_id, rating, Utc::now())
}

pub fn answered_at(quiz_id: &str, rating: Rating, ts: DateTime<Utc>) -> Event {
    Event {
        ts,
        kind: EventKind::QuizAnswered,
        path: PATH_ID.into(),
        atom: None,
        quiz: Some(quiz_id.into()),
        payload: EventPayload {
            rating: Some(rating),
            ..Default::default()
        },
    }
}

pub fn presented(quiz_id: &str) -> Event {
    presented_at(quiz_id, Utc::now())
}

pub fn presented_at(quiz_id: &str, ts: DateTime<Utc>) -> Event {
    Event {
        ts,
        kind: EventKind::QuizPresented,
        path: PATH_ID.into(),
        atom: None,
        quiz: Some(quiz_id.into()),
        payload: EventPayload::default(),
    }
}

/// The set of events that mark `atom_id` complete:
/// lesson taught + an answer on each of its quizzes.
pub fn complete_events(atom_id: &str) -> Vec<Event> {
    vec![
        taught(atom_id),
        answered(&format!("{atom_id}.q1"), Rating::Good),
        answered(&format!("{atom_id}.q2"), Rating::Good),
        answered(&format!("{atom_id}.q3"), Rating::Good),
    ]
}

// ── Progress ────────────────────────────────────────────────────────

/// Fold a synthetic event log into a `PathProgress`. Mirrors
/// `PathProgress::load` from `events` and `cards` to let tests
/// simulate the behavior without a database.
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
