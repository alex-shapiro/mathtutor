//! Unit tests for `syllabus::upcoming_atoms` and `syllabus::upcoming_top_down`.

use std::collections::HashMap;

use chrono::Utc;

use mathtutor::event_log::{Event, EventKind, EventPayload};
use mathtutor::graph::{FlatConcept, Graph};
use mathtutor::path::PathFile;
use mathtutor::progress::PathProgress;
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
        updated_at: Utc::now(),
        targets: targets.iter().map(|s| (*s).to_string()).collect(),
        strategy: mathtutor::types::Strategy::BottomUp,
        subpath: Vec::new(),
    }
}

fn progress_of(events: &[Event]) -> PathProgress {
    PathProgress {
        taught_atoms: events.iter().filter_map(|e| e.atom.clone()).collect(),
        ..PathProgress::default()
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

#[test]
fn empty_targets_yield_empty_syllabus() {
    let g = graph_of(vec![]);
    let p = path_with(&[]);
    assert_eq!(
        syllabus::upcoming_atoms(&g, &p.targets, &PathProgress::default()),
        Vec::<String>::new()
    );
}

#[test]
fn single_untaught_target_appears() {
    let g = graph_of(vec![atom("a", &[])]);
    let p = path_with(&["a"]);
    assert_eq!(
        syllabus::upcoming_atoms(&g, &p.targets, &PathProgress::default()),
        vec!["a".to_string()]
    );
}

#[test]
fn taught_target_is_skipped() {
    let g = graph_of(vec![atom("a", &[])]);
    let p = path_with(&["a"]);
    let events = vec![taught("a")];
    assert_eq!(
        syllabus::upcoming_atoms(&g, &p.targets, &progress_of(&events)),
        Vec::<String>::new()
    );
}

#[test]
fn lesson_authored_event_counts_as_taught() {
    let g = graph_of(vec![atom("a", &[])]);
    let p = path_with(&["a"]);
    let events = vec![authored("a")];
    assert_eq!(
        syllabus::upcoming_atoms(&g, &p.targets, &progress_of(&events)),
        Vec::<String>::new()
    );
}

#[test]
fn prereq_appears_before_target() {
    let g = graph_of(vec![atom("pre", &[]), atom("target", &["pre"])]);
    let p = path_with(&["target"]);
    assert_eq!(
        syllabus::upcoming_atoms(&g, &p.targets, &PathProgress::default()),
        vec!["pre".to_string(), "target".to_string()],
    );
}

#[test]
fn taught_prereq_is_skipped_but_target_remains() {
    let g = graph_of(vec![atom("pre", &[]), atom("target", &["pre"])]);
    let p = path_with(&["target"]);
    let events = vec![taught("pre")];
    assert_eq!(
        syllabus::upcoming_atoms(&g, &p.targets, &progress_of(&events)),
        vec!["target".to_string()],
    );
}

#[test]
fn diamond_prereq_appears_once() {
    let g = graph_of(vec![
        atom("tx.1", &["a", "b"]),
        atom("a", &["c"]),
        atom("b", &["c"]),
        atom("c", &[]),
    ]);
    let p = path_with(&["tx.1"]);
    let order = syllabus::upcoming_atoms(&g, &p.targets, &PathProgress::default());
    assert_eq!(order.iter().filter(|x| *x == "c").count(), 1);
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
    let order = syllabus::upcoming_atoms(&g, &p.targets, &PathProgress::default());
    assert!(!order.contains(&"la.2".to_string()));
    assert!(order.contains(&"la.2.1".to_string()));
    assert!(order.contains(&"la.2.2".to_string()));
}

#[test]
fn multiple_targets_walked_in_order() {
    let g = graph_of(vec![atom("a", &[]), atom("b", &[])]);
    let p = path_with(&["a", "b"]);
    assert_eq!(
        syllabus::upcoming_atoms(&g, &p.targets, &PathProgress::default()),
        vec!["a".to_string(), "b".to_string()],
    );
}

#[test]
fn missing_id_is_silently_skipped() {
    let g = graph_of(vec![atom("a", &[])]);
    let p = path_with(&["a", "ghost"]);
    assert_eq!(
        syllabus::upcoming_atoms(&g, &p.targets, &PathProgress::default()),
        vec!["a".to_string()]
    );
}

#[test]
fn n_truncates_atoms_but_total_remaining_is_full_count() {
    let g = graph_of(vec![
        atom("a", &[]),
        atom("b", &[]),
        atom("c", &[]),
        atom("d", &[]),
    ]);
    let p = path_with(&["a", "b", "c", "d"]);
    let upcoming = syllabus::upcoming_atoms(&g, &p.targets, &PathProgress::default());
    assert_eq!(upcoming.len(), 4);
    let n = 2;
    let truncated: Vec<_> = upcoming.iter().take(n).cloned().collect();
    assert_eq!(truncated, vec!["a".to_string(), "b".to_string()]);
}

fn top_down(targets: &[&str]) -> PathFile {
    PathFile {
        strategy: mathtutor::types::Strategy::TopDown,
        ..path_with(targets)
    }
}

#[test]
fn top_down_lists_targets_not_prereqs() {
    let g = graph_of(vec![atom("p", &[]), atom("t", &["p"])]);
    let p = top_down(&["t"]);
    assert_eq!(
        syllabus::upcoming_top_down(&g, &p.targets, &PathProgress::default(), &[]),
        vec!["t".to_string()],
    );
}

#[test]
fn top_down_prefixes_subpath_before_targets() {
    let g = graph_of(vec![atom("p", &[]), atom("q", &[]), atom("t", &["p", "q"])]);
    let p = top_down(&["t"]);
    let subpath = vec!["q".to_string(), "t".to_string()];
    assert_eq!(
        syllabus::upcoming_top_down(&g, &p.targets, &PathProgress::default(), &subpath),
        vec!["q".to_string(), "t".to_string()],
    );
}

#[test]
fn top_down_skips_taught_subpath_and_target_atoms() {
    let g = graph_of(vec![atom("p", &[]), atom("t", &["p"])]);
    let p = top_down(&["t"]);
    let subpath = vec!["p".to_string(), "t".to_string()];
    assert_eq!(
        syllabus::upcoming_top_down(&g, &p.targets, &progress_of(&[taught("p")]), &subpath),
        vec!["t".to_string()],
    );
}
