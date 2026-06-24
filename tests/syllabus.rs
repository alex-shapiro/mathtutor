//! Tests for `mt path syllabus` / `GetSyllabus`: the forward-looking
//! lookahead over upcoming lesson topics. Mirrors `atom_completion.rs`
//! in its in-memory fixture style — no DB, just the pure walker.

use std::collections::HashMap;

use chrono::Utc;

use mathtutor::event_log::{Event, EventKind, EventPayload};
use mathtutor::graph::{FlatConcept, Graph};
use mathtutor::path::PathFile;
use mathtutor::syllabus;

const PATH_ID: &str = "p_test";

fn atom(id: &str, prereqs: &[&str]) -> FlatConcept {
    FlatConcept {
        id: id.into(),
        name: format!("Atom {id}"),
        description: Some(format!("desc {id}")),
        prerequisites: prereqs.iter().map(|s| (*s).to_string()).collect(),
        children_ids: Vec::new(),
        lesson: None,
        quizzes: Vec::new(),
    }
}

fn cluster(id: &str, children: &[&str]) -> FlatConcept {
    FlatConcept {
        id: id.into(),
        name: format!("Cluster {id}"),
        description: None,
        prerequisites: Vec::new(),
        children_ids: children.iter().map(|s| (*s).to_string()).collect(),
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
        strategy: mathtutor::types::Strategy::BottomUp,
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

fn authored(atom_id: &str) -> Event {
    Event {
        ts: Utc::now(),
        kind: EventKind::LessonAuthored,
        path: PATH_ID.into(),
        atom: Some(atom_id.into()),
        quiz: None,
        payload: EventPayload::default(),
    }
}

// ── upcoming_atoms walker contract ─────────────────────────────────

#[test]
fn empty_targets_yield_empty_syllabus() {
    let g = graph_of(vec![]);
    let p = path_with(&[]);
    assert!(syllabus::upcoming_atoms(&g, &p, &[]).is_empty());
}

#[test]
fn single_untaught_target_appears() {
    let g = graph_of(vec![atom("a", &[])]);
    let p = path_with(&["a"]);
    assert_eq!(syllabus::upcoming_atoms(&g, &p, &[]), vec!["a".to_string()]);
}

#[test]
fn taught_target_is_skipped() {
    // Forward-looking only: once the lesson is taught, the atom drops
    // out of the syllabus regardless of any pending quiz work.
    let g = graph_of(vec![atom("a", &[])]);
    let p = path_with(&["a"]);
    let events = vec![taught("a")];
    assert!(syllabus::upcoming_atoms(&g, &p, &events).is_empty());
}

#[test]
fn lesson_authored_event_counts_as_taught() {
    // Authoring a lesson implies presenting it — matches the
    // `lesson_taught_in_path` semantics the scheduler uses.
    let g = graph_of(vec![atom("a", &[])]);
    let p = path_with(&["a"]);
    let events = vec![authored("a")];
    assert!(syllabus::upcoming_atoms(&g, &p, &events).is_empty());
}

#[test]
fn prereq_appears_before_target() {
    let g = graph_of(vec![atom("pre", &[]), atom("target", &["pre"])]);
    let p = path_with(&["target"]);
    assert_eq!(
        syllabus::upcoming_atoms(&g, &p, &[]),
        vec!["pre".to_string(), "target".to_string()],
    );
}

#[test]
fn taught_prereq_is_skipped_but_target_remains() {
    let g = graph_of(vec![atom("pre", &[]), atom("target", &["pre"])]);
    let p = path_with(&["target"]);
    let events = vec![taught("pre")];
    assert_eq!(
        syllabus::upcoming_atoms(&g, &p, &events),
        vec!["target".to_string()],
    );
}

#[test]
fn diamond_prereq_appears_once() {
    // tx.1 needs both `a` and `b`; both need `c`. `c` must appear once.
    let g = graph_of(vec![
        atom("tx.1", &["a", "b"]),
        atom("a", &["c"]),
        atom("b", &["c"]),
        atom("c", &[]),
    ]);
    let p = path_with(&["tx.1"]);
    let order = syllabus::upcoming_atoms(&g, &p, &[]);
    assert_eq!(order.iter().filter(|x| *x == "c").count(), 1);
    // c precedes a and b, which precede tx.1.
    let pos = |id: &str| order.iter().position(|x| x == id).unwrap();
    assert!(pos("c") < pos("a"));
    assert!(pos("c") < pos("b"));
    assert!(pos("a") < pos("tx.1"));
    assert!(pos("b") < pos("tx.1"));
}

#[test]
fn cluster_target_expands_to_atomic_children() {
    let g = graph_of(vec![
        cluster("la.2", &["la.2.1", "la.2.2"]),
        atom("la.2.1", &[]),
        atom("la.2.2", &[]),
    ]);
    let p = path_with(&["la.2"]);
    let order = syllabus::upcoming_atoms(&g, &p, &[]);
    // Cluster itself doesn't appear; its leaves do.
    assert!(!order.contains(&"la.2".to_string()));
    assert!(order.contains(&"la.2.1".to_string()));
    assert!(order.contains(&"la.2.2".to_string()));
}

#[test]
fn multiple_targets_walked_in_order() {
    let g = graph_of(vec![atom("a", &[]), atom("b", &[])]);
    let p = path_with(&["a", "b"]);
    assert_eq!(
        syllabus::upcoming_atoms(&g, &p, &[]),
        vec!["a".to_string(), "b".to_string()],
    );
}

#[test]
fn missing_id_is_silently_skipped() {
    // A target that's not in the graph (e.g. graph was reshipped without
    // that atom) shouldn't crash the walker — just drop out.
    let g = graph_of(vec![atom("a", &[])]);
    let p = path_with(&["a", "ghost"]);
    assert_eq!(syllabus::upcoming_atoms(&g, &p, &[]), vec!["a".to_string()]);
}

// ── compute_syllabus view shape (without DB: spot-check truncation) ──
//
// `compute_syllabus` itself needs a libSQL connection, so the
// truncation logic is exercised separately by re-running the walker
// here and truncating in the same way the public function does.

#[test]
fn n_truncates_atoms_but_total_remaining_is_full_count() {
    let g = graph_of(vec![
        atom("a", &[]),
        atom("b", &[]),
        atom("c", &[]),
        atom("d", &[]),
    ]);
    let p = path_with(&["a", "b", "c", "d"]);
    let upcoming = syllabus::upcoming_atoms(&g, &p, &[]);
    assert_eq!(upcoming.len(), 4);
    // The CLI/MCP layer truncates with `.take(n)`; verify the count
    // contract: total_remaining is the untruncated length.
    let n = 2;
    let truncated: Vec<_> = upcoming.iter().take(n).cloned().collect();
    assert_eq!(truncated, vec!["a".to_string(), "b".to_string()]);
}
