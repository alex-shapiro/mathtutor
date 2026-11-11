//! Tests for `tree::compute_tree`, its text rendering, badges, and spine.

use std::collections::HashSet;

use mathtutor::graph::{FlatConcept, Graph};
use mathtutor::progress::{AtomStatus, PathProgress};
use mathtutor::tree::{self, TreeNode, build_spine};
use mathtutor::types::{Difficulty, Rating, Strategy};

mod common;

use common::{atom, cluster, quiz};

fn state_badge(progress: &PathProgress, c: &FlatConcept) -> String {
    AtomStatus::of(progress, c).badge()
}

fn find<'a>(nodes: &'a [TreeNode], id: &str) -> Option<&'a TreeNode> {
    nodes
        .iter()
        .find_map(|n| (n.id == id).then_some(n).or_else(|| find(&n.children, id)))
}

#[tokio::test]
async fn compute_tree_reports_progress_targets_and_next_atom() {
    let db = common::empty_db().await;
    let id = mathtutor::path::cmd_path_new(&db, "g", &["fnd.1.1.2".into()], Strategy::BottomUp)
        .await
        .unwrap();
    let t = tree::compute_tree(&db, Some(&id), None).await.unwrap();

    assert_eq!((t.targets.total, t.targets.learned_pct), (1, 0));
    assert_eq!(t.reachable.total, 2);
    assert_eq!(t.next.as_deref(), Some("fnd.1.1.1"));
    assert_eq!(t.areas.len(), 1);
    assert_eq!(t.areas[0].prefix, "fnd");
    let target = find(&t.areas[0].nodes, "fnd.1.1.2").expect("target node");
    assert!(target.is_target && target.is_atom);
    assert!(!find(&t.areas[0].nodes, "fnd.1.1.1").unwrap().is_target);

    let mut text = Vec::new();
    tree::write_tree(&mut text, &t).unwrap();
    let text = String::from_utf8(text).unwrap();
    assert!(text.contains("targets:     0 / 1 learned (0%)"), "{text}");
    let line = |id: &str| text.lines().find(|l| l.contains(id)).unwrap().to_string();
    assert!(line("fnd.1.1.1 ").ends_with("← NEXT"), "{text}");
    assert!(line("fnd.1.1.2 ").ends_with(" *"), "{text}");
}

#[tokio::test]
async fn compute_tree_stops_at_the_requested_depth() {
    let db = common::empty_db().await;
    let id = mathtutor::path::cmd_path_new(&db, "g", &["fnd.1.1.2".into()], Strategy::BottomUp)
        .await
        .unwrap();
    let t = tree::compute_tree(&db, Some(&id), Some(1)).await.unwrap();
    let top = &t.areas[0].nodes;
    assert!(!top.is_empty());
    assert!(top.iter().all(|n| n.children.is_empty()));
}

#[test]
fn badge_empty_if_atom_untaught() {
    let a = atom("a", &[], None, vec![]);
    assert_eq!(state_badge(&PathProgress::default(), &a), "[····]");
}

#[test]
fn badge_lesson_slot_only_lights_up_after_taught() {
    let a = atom("a", &[], Some("body"), vec![]);
    assert_eq!(state_badge(&PathProgress::default(), &a), "[····]");
    let events = vec![common::taught("a")];
    assert_eq!(state_badge(&common::progress_of(&events), &a), "[L···]");
}

#[test]
fn badge_lowercase_if_quiz_authored_but_unanswered() {
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
fn badge_uppercase_if_quiz_answered_correctly() {
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
        common::answered("a.q3", Rating::Again),
    ];
    assert_eq!(state_badge(&common::progress_of(&events), &a), "[LEMh]");
}

#[test]
fn spine_includes_atoms_and_existing_ancestors() {
    let g = Graph::of_concepts(vec![
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
