//! Tests for `state::compute_progress` — the per-path target and
//! reachable-atom counters surfaced by `mt path state` and the MCP
//! `get_state` tool.

use std::collections::HashMap;

use chrono::Utc;

use mathtutor::event_log::{Event, EventKind, EventPayload};
use mathtutor::graph::{FlatConcept, Graph, Quiz};
use mathtutor::path::PathFile;
use mathtutor::progress::PathProgress;
use mathtutor::state;
use mathtutor::types::{Difficulty, Rating};

const PATH_ID: &str = "p_test";

fn quiz(id: &str, difficulty: Difficulty) -> Quiz {
    Quiz {
        id: id.into(),
        difficulty,
        kind: None,
        question: format!("q? {id}"),
        answer: "a".into(),
        rubric: None,
    }
}

fn complete_atom(id: &str, prereqs: &[&str]) -> FlatConcept {
    FlatConcept {
        id: id.into(),
        name: id.into(),
        description: None,
        prerequisites: prereqs.iter().map(|s| (*s).to_string()).collect(),
        children_ids: Vec::new(),
        lesson: Some("body".into()),
        quizzes: vec![
            quiz(&format!("{id}.q1"), Difficulty::Easy),
            quiz(&format!("{id}.q2"), Difficulty::Medium),
            quiz(&format!("{id}.q3"), Difficulty::Hard),
        ],
    }
}

fn empty_atom(id: &str, prereqs: &[&str]) -> FlatConcept {
    FlatConcept {
        id: id.into(),
        name: id.into(),
        description: None,
        prerequisites: prereqs.iter().map(|s| (*s).to_string()).collect(),
        children_ids: Vec::new(),
        lesson: None,
        quizzes: Vec::new(),
    }
}

fn graph_of(concepts: Vec<FlatConcept>) -> Graph {
    let mut by_id = HashMap::new();
    for c in concepts {
        by_id.insert(c.id.clone(), c);
    }
    Graph { by_id }
}

fn path_with(targets: &[&str]) -> PathFile {
    PathFile {
        id: PATH_ID.into(),
        goal: "test".into(),
        created_at: Utc::now(),
        target_atoms: targets.iter().map(|s| (*s).to_string()).collect(),
    }
}

fn taught(atom_id: &str) -> Event {
    Event {
        ts: Utc::now(),
        kind: EventKind::LessonTaught,
        path: PATH_ID.into(),
        atom: Some(atom_id.into()),
        quiz: None,
        payload: EventPayload::default(),
    }
}

fn answered(quiz_id: &str, rating: Rating) -> Event {
    Event {
        ts: Utc::now(),
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

/// Mark a fully-equipped atom as complete: lesson taught, all three
/// difficulty quizzes answered correctly at least once.
fn complete(atom_id: &str) -> Vec<Event> {
    vec![
        taught(atom_id),
        answered(&format!("{atom_id}.q1"), Rating::Good),
        answered(&format!("{atom_id}.q2"), Rating::Good),
        answered(&format!("{atom_id}.q3"), Rating::Good),
    ]
}

#[test]
fn target_complete_counts_toward_both_targets_and_reachable() {
    let g = graph_of(vec![complete_atom("a", &[])]);
    let p = path_with(&["a"]);
    let events = complete("a");

    let (t, r) = state::compute_progress(&g, &p, &PathProgress::from_events(&events));
    assert_eq!(t.total, 1);
    assert_eq!(t.learned, 1);
    assert_eq!(t.learned_pct, 100);
    assert_eq!(r.total, 1);
    assert_eq!(r.taught, 1);
    assert_eq!(r.learned, 1);
}

#[test]
fn prereq_complete_counts_toward_reachable_only() {
    // Targets only count completed targets; prereqs that aren't also
    // targets contribute only to `reachable.learned`.
    let g = graph_of(vec![
        complete_atom("pre", &[]),
        complete_atom("target", &["pre"]),
    ]);
    let p = path_with(&["target"]);
    let events = complete("pre");

    let (t, r) = state::compute_progress(&g, &p, &PathProgress::from_events(&events));
    assert_eq!(t.total, 1);
    assert_eq!(t.learned, 0, "target itself is not yet complete");
    assert_eq!(t.learned_pct, 0);
    assert_eq!(r.total, 2);
    assert_eq!(r.learned, 1, "prereq counts as a reachable learned atom");
}

#[test]
fn taught_counts_lesson_taught_in_path_regardless_of_quiz_progress() {
    // Lesson presented but no quizzes answered yet: the atom is taught
    // but not learned. Reachable.taught must reflect lesson exposure
    // independent of completion.
    let g = graph_of(vec![complete_atom("a", &[])]);
    let p = path_with(&["a"]);
    let events = vec![taught("a")];

    let (t, r) = state::compute_progress(&g, &p, &PathProgress::from_events(&events));
    assert_eq!(t.learned, 0);
    assert_eq!(r.taught, 1);
    assert_eq!(r.learned, 0);
}

#[test]
fn empty_path_yields_zero_progress_without_division_panic() {
    let g = graph_of(vec![]);
    let p = path_with(&[]);

    let (t, r) = state::compute_progress(&g, &p, &PathProgress::default());
    assert_eq!(t.total, 0);
    assert_eq!(t.learned, 0);
    assert_eq!(t.learned_pct, 0, "no targets must not divide by zero");
    assert_eq!(r.total, 0);
    assert_eq!(r.taught, 0);
    assert_eq!(r.learned, 0);
}

#[test]
fn reachable_includes_transitive_prereqs() {
    // a ← b ← c (target). Reachable should be {a, b, c} = 3.
    let g = graph_of(vec![
        empty_atom("a", &[]),
        empty_atom("b", &["a"]),
        empty_atom("c", &["b"]),
    ]);
    let p = path_with(&["c"]);

    let (_t, r) = state::compute_progress(&g, &p, &PathProgress::default());
    assert_eq!(r.total, 3);
}

#[test]
fn learned_pct_rounds_down() {
    // 1 of 3 targets learned → 33 (integer division), not 34.
    let g = graph_of(vec![
        complete_atom("a", &[]),
        complete_atom("b", &[]),
        complete_atom("c", &[]),
    ]);
    let p = path_with(&["a", "b", "c"]);
    let events = complete("a");

    let (t, _r) = state::compute_progress(&g, &p, &PathProgress::from_events(&events));
    assert_eq!(t.learned, 1);
    assert_eq!(t.learned_pct, 33);
}
