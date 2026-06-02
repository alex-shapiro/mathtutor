//! Tests for `Graph::atom`, `Graph::quiz`, and `Graph::reachable_atoms`.

use mathtutor::Error;
use mathtutor::types::Difficulty;

mod common;

use common::{atom, cluster, graph_of, quiz};

fn graph_with_quiz(atom_id: &str, quiz_id: &str) -> mathtutor::graph::Graph {
    graph_of(vec![atom(
        atom_id,
        &[],
        Some("body"),
        vec![quiz(quiz_id, Difficulty::Easy)],
    )])
}

// ── Graph::quiz ─────────────────────────────────────────────────────

#[test]
fn quiz_returns_atom_and_quiz_for_valid_id() {
    let g = graph_with_quiz("fnd.1.1.1", "fnd.1.1.1.q1");
    let (a, q) = g.quiz("fnd.1.1.1.q1").expect("valid");
    assert_eq!(a.id, "fnd.1.1.1");
    assert_eq!(q.id, "fnd.1.1.1.q1");
}

#[test]
fn quiz_rejects_malformed_id() {
    let g = graph_with_quiz("fnd.1.1.1", "fnd.1.1.1.q1");
    // Missing `.qN` suffix → can't derive an atom id.
    assert!(matches!(g.quiz("fnd.1.1.1"), Err(Error::UnknownId(_))));
}

#[test]
fn quiz_rejects_unknown_atom() {
    let g = graph_with_quiz("fnd.1.1.1", "fnd.1.1.1.q1");
    assert!(matches!(g.quiz("nope.1.q1"), Err(Error::AtomNotFound(_))));
}

#[test]
fn quiz_rejects_unknown_quiz_on_known_atom() {
    // Atom is real but doesn't own a `.q9` quiz — the most likely typo
    // path (right atom, wrong index).
    let g = graph_with_quiz("fnd.1.1.1", "fnd.1.1.1.q1");
    assert!(matches!(g.quiz("fnd.1.1.1.q9"), Err(Error::UnknownId(_))));
}

// ── Graph::atom ─────────────────────────────────────────────────────

#[test]
fn atom_accepts_leaf_concept() {
    let g = graph_with_quiz("fnd.1.1.1", "fnd.1.1.1.q1");
    let a = g.atom("fnd.1.1.1").expect("valid");
    assert_eq!(a.id, "fnd.1.1.1");
}

#[test]
fn atom_rejects_cluster() {
    let g = graph_of(vec![cluster("fnd.1", &["fnd.1.1"])]);
    assert!(matches!(g.atom("fnd.1"), Err(Error::NotAtom(_))));
}

#[test]
fn atom_rejects_unknown_id() {
    let g = graph_with_quiz("fnd.1.1.1", "fnd.1.1.1.q1");
    assert!(matches!(g.atom("nope"), Err(Error::AtomNotFound(_))));
}

// ── Graph::reachable_atoms ──────────────────────────────────────────

#[test]
fn reachable_includes_transitive_prereqs() {
    // tx.1.1 → la.2.2 → la.1.1 ; tx.1.1 → fnd.2.3.1 (different area).
    // All four should be reachable from a single tx.1.1 target.
    let g = graph_of(vec![
        common::empty_atom("tx.1.1", &["la.2.2", "fnd.2.3.1"]),
        common::empty_atom("la.2.2", &["la.1.1"]),
        common::empty_atom("la.1.1", &[]),
        common::empty_atom("fnd.2.3.1", &[]),
    ]);
    let reach = g.reachable_atoms(&["tx.1.1".to_string()]);
    assert!(reach.contains("tx.1.1"));
    assert!(reach.contains("la.2.2"));
    assert!(reach.contains("la.1.1"));
    assert!(reach.contains("fnd.2.3.1"));
}

#[test]
fn reachable_expands_cluster_prereqs_to_their_atoms() {
    // tx.1.1's prereq points at the cluster `la.2.2`, not a specific
    // atom. The cluster has two leaves; both must end up reachable.
    let g = graph_of(vec![
        common::empty_atom("tx.1.1", &["la.2.2"]),
        cluster("la.2.2", &["la.2.2.1", "la.2.2.2"]),
        common::empty_atom("la.2.2.1", &[]),
        common::empty_atom("la.2.2.2", &[]),
    ]);
    let reach = g.reachable_atoms(&["tx.1.1".to_string()]);
    assert!(reach.contains("tx.1.1"));
    assert!(reach.contains("la.2.2.1"));
    assert!(reach.contains("la.2.2.2"));
    // The cluster itself is not an atom and must not appear.
    assert!(!reach.contains("la.2.2"));
}

#[test]
fn reachable_handles_diamond_without_looping() {
    // tx.1 → A, tx.1 → B ; A → C, B → C. C must show up exactly once.
    let g = graph_of(vec![
        common::empty_atom("tx.1", &["a", "b"]),
        common::empty_atom("a", &["c"]),
        common::empty_atom("b", &["c"]),
        common::empty_atom("c", &[]),
    ]);
    let reach = g.reachable_atoms(&["tx.1".to_string()]);
    assert_eq!(reach.len(), 4);
}
