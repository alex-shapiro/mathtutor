//! Tests for `state::compute_progress` — the per-path target and
//! reachable-atom counters surfaced by `mt path state` and the MCP
//! `get_state` tool.

use mathtutor::progress::PathProgress;
use mathtutor::state;

mod common;

use common::{
    complete_atom, complete_events, empty_atom, graph_of, path_with, progress_of, taught,
};

#[test]
fn target_complete_counts_toward_both_targets_and_reachable() {
    let g = graph_of(vec![complete_atom("a", &[])]);
    let p = path_with(&["a"]);
    let events = complete_events("a");

    let (t, r) = state::compute_progress(&g, &p, &progress_of(&events));
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
    let events = complete_events("pre");

    let (t, r) = state::compute_progress(&g, &p, &progress_of(&events));
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

    let (t, r) = state::compute_progress(&g, &p, &progress_of(&events));
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
    let events = complete_events("a");

    let (t, _r) = state::compute_progress(&g, &p, &progress_of(&events));
    assert_eq!(t.learned, 1);
    assert_eq!(t.learned_pct, 33);
}
