//! Tests for `tree::state_badge` (per-atom `[LEMH]` glyph) and
//! `tree::build_spine` (cluster ancestors needed to root the tree view).

use std::collections::HashSet;

use mathtutor::progress::PathProgress;
use mathtutor::tree::{build_spine, state_badge};
use mathtutor::types::{Difficulty, Rating};

mod common;

use common::{atom, cluster, graph_of, quiz};

// ── state_badge ─────────────────────────────────────────────────────

#[test]
fn badge_empty_when_nothing_stored() {
    let a = atom("a", &[], None, vec![]);
    assert_eq!(state_badge(&PathProgress::default(), &a), "[····]");
}

#[test]
fn badge_lesson_slot_only_lights_up_after_taught() {
    let a = atom("a", &[], Some("body"), vec![]);
    // Body exists in the graph but no `LessonTaught` event for this
    // path yet — `L` stays unlit.
    assert_eq!(state_badge(&PathProgress::default(), &a), "[····]");
    let events = vec![common::taught("a")];
    assert_eq!(state_badge(&common::progress_of(&events), &a), "[L···]");
}

#[test]
fn badge_lowercase_when_quiz_authored_but_unanswered() {
    let a = atom(
        "a",
        &[],
        Some("body"),
        vec![
            quiz("a.q1", Difficulty::Easy),
            quiz("a.q2", Difficulty::Medium),
        ],
    );
    let events = vec![common::taught("a")];
    assert_eq!(state_badge(&common::progress_of(&events), &a), "[Lem·]");
}

#[test]
fn badge_uppercase_when_quiz_answered_correctly() {
    let a = atom(
        "a",
        &[],
        Some("body"),
        vec![
            quiz("a.q1", Difficulty::Easy),
            quiz("a.q2", Difficulty::Medium),
            quiz("a.q3", Difficulty::Hard),
        ],
    );
    let events = vec![
        common::taught("a"),
        common::answered("a.q1", Rating::Good),
        common::answered("a.q2", Rating::Easy),
        common::answered("a.q3", Rating::Again), // wrong → stays lowercase
    ];
    assert_eq!(state_badge(&common::progress_of(&events), &a), "[LEMh]");
}

// ── build_spine ─────────────────────────────────────────────────────

#[test]
fn spine_includes_atoms_and_existing_ancestors() {
    // Cluster `la.5` exists but the bare prefix `la` is absent from this
    // graph. Spine should include the existing ancestors and stop at the
    // missing one.
    let g = graph_of(vec![
        cluster("la.5", &["la.5.4"]),
        cluster("la.5.4", &["la.5.4.7"]),
        common::empty_atom("la.5.4.7", &[]),
    ]);
    let mut atoms = HashSet::new();
    atoms.insert("la.5.4.7".to_string());
    let spine = build_spine(&g, &atoms);
    assert!(spine.contains("la.5.4.7"));
    assert!(spine.contains("la.5.4"));
    assert!(spine.contains("la.5"));
    assert!(!spine.contains("la"));
}
