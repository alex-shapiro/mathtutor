//! Tests for `Graph::atom`, `Graph::quiz`, `Graph::reachable_atoms`,
//! and the `mt graph check` orphan detector.

use std::fs;

use mathtutor::Error;
use mathtutor::graph;
use mathtutor::types::Difficulty;
use tempfile::TempDir;

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

// ── graph check: orphan detection ───────────────────────────────────

/// Write a minimal one-area graph dir with the given `area_body` (the
/// content of the area file's `children:` block) and return the
/// tempdir to keep it alive for the test.
fn write_graph(area_body: &str) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let areas = dir.path().join("areas");
    fs::create_dir(&areas).expect("areas/");
    let manifest = "
schema_version: 1
areas:
  - prefix: ta
    slug: test-area
    file: areas/test.ayml
    summary: \"t\"
";
    fs::write(dir.path().join("manifest.ayml"), manifest).expect("manifest");
    let area = format!(
        "
schema_version: 2
area: test-area
prefix: ta
summary: \"t\"
motivation: \"t\"
children:
{area_body}"
    );
    fs::write(areas.join("test.ayml"), area).expect("area");
    dir
}

fn orphan_ids(report: &graph::CheckReport) -> Vec<String> {
    report
        .issues
        .iter()
        .filter(|i| i.message.starts_with("orphan atom"))
        .filter_map(|i| i.node.clone())
        .collect()
}

#[test]
fn orphan_check_flags_unreferenced_atom() {
    // ta.1.1 is nobody's prerequisite — it must be reported.
    let dir = write_graph(
        "
  - id: ta.1
    name: cluster
    children:
      - id: ta.1.1
        name: lonely atom
        description: nobody cites me
",
    );
    let report = graph::run_check(Some(dir.path())).expect("run_check");
    assert_eq!(orphan_ids(&report), vec!["ta.1.1".to_string()]);
    // The issue message must include the human-readable name so the
    // operator can scan output without cross-referencing the file.
    let msg = &report
        .issues
        .iter()
        .find(|i| i.node.as_deref() == Some("ta.1.1"))
        .expect("issue")
        .message;
    assert!(msg.contains("lonely atom"), "got: {msg}");
}

#[test]
fn orphan_check_skips_terminal_atoms() {
    // Same shape as above but the atom opts out via `terminal: true`.
    let dir = write_graph(
        "
  - id: ta.1
    name: cluster
    children:
      - id: ta.1.1
        name: culminating topic
        description: end of the line, by design
        terminal: true
",
    );
    let report = graph::run_check(Some(dir.path())).expect("run_check");
    assert!(
        orphan_ids(&report).is_empty(),
        "terminal:true must suppress: {:?}",
        report.issues
    );
}

#[test]
fn orphan_check_clears_when_referenced_as_prereq() {
    // ta.1.1 is a prereq of ta.1.2 — must not be flagged.
    // ta.1.2 itself has no downstream, so it WILL be an orphan; assert
    // that's the only one.
    let dir = write_graph(
        "
  - id: ta.1
    name: cluster
    children:
      - id: ta.1.1
        name: foundational
        description: cited downstream
      - id: ta.1.2
        name: builds on it
        description: top of the chain
        prerequisites:
          - ta.1.1
",
    );
    let report = graph::run_check(Some(dir.path())).expect("run_check");
    assert_eq!(orphan_ids(&report), vec!["ta.1.2".to_string()]);
}

#[test]
fn orphan_check_does_not_flag_clusters() {
    // ta.1 is a cluster (has children). Even though no concept lists
    // ta.1 as a prerequisite, clusters must never be flagged — only
    // atoms (leaves of the concept tree) participate in the check.
    let dir = write_graph(
        "
  - id: ta.1
    name: cluster
    children:
      - id: ta.1.1
        name: only atom
        description: keeps things minimal
        terminal: true
",
    );
    let report = graph::run_check(Some(dir.path())).expect("run_check");
    assert!(orphan_ids(&report).is_empty(), "{:?}", report.issues);
}

#[test]
fn orphan_check_cluster_prereq_covers_descendant_atoms() {
    // ta.2.1 depends on the cluster ta.1, not on its individual
    // children. The orphan check must treat ta.1.1 and ta.1.2 as
    // covered — same semantics as `Graph::reachable_atoms`, which
    // expands cluster prereqs into their atoms for the scheduler.
    let dir = write_graph(
        "
  - id: ta.1
    name: covered cluster
    children:
      - id: ta.1.1
        name: cluster child A
        description: nothing cites this atom directly
      - id: ta.1.2
        name: cluster child B
        description: nothing cites this atom directly
  - id: ta.2
    name: downstream cluster
    children:
      - id: ta.2.1
        name: cites the cluster
        description: pulls in the whole prereq topic at once
        terminal: true
        prerequisites:
          - ta.1
",
    );
    let report = graph::run_check(Some(dir.path())).expect("run_check");
    assert!(orphan_ids(&report).is_empty(), "{:?}", report.issues);
}
