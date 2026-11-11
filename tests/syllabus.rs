//! Tests for `syllabus::upcoming` and the agreement of the syllabus with the scheduler.

use mathtutor::db::{Db, DbConfig};
use mathtutor::graph::Curriculum;
use mathtutor::graph::Graph;
use mathtutor::overlay::QuizDraft;
use mathtutor::progress::PathProgress;
use mathtutor::scheduler::{self, Action, NextMode};
use mathtutor::surface::Surface;
use mathtutor::types::{QuizType, Rating, Strategy};
use mathtutor::{answer, path, store, subpath, syllabus};

mod common;

use common::{
    cluster, empty_atom as atom, graph_of, lesson_authored, path_with, path_with_strategy, taught,
};

fn upcoming_atoms(g: &Graph, targets: &[String], progress: &PathProgress) -> Vec<String> {
    syllabus::upcoming(g, Strategy::BottomUp, targets, &[], progress)
}

fn upcoming_top_down(
    g: &Graph,
    targets: &[String],
    progress: &PathProgress,
    subpath: &[String],
) -> Vec<String> {
    syllabus::upcoming(g, Strategy::TopDown, targets, subpath, progress)
}

#[test]
fn empty_targets_yield_empty_syllabus() {
    let g = graph_of(vec![]);
    let p = path_with(&[]);
    assert_eq!(
        upcoming_atoms(&g, &p.targets, &PathProgress::default()),
        Vec::<String>::new()
    );
}

#[test]
fn single_untaught_target_appears() {
    let g = graph_of(vec![atom("a", &[])]);
    let p = path_with(&["a"]);
    assert_eq!(
        upcoming_atoms(&g, &p.targets, &PathProgress::default()),
        vec!["a".to_string()]
    );
}

#[test]
fn taught_target_is_skipped() {
    let g = graph_of(vec![atom("a", &[])]);
    let p = path_with(&["a"]);
    let events = vec![taught("a")];
    assert_eq!(
        upcoming_atoms(&g, &p.targets, &common::progress_of(&events)),
        Vec::<String>::new()
    );
}

#[test]
fn lesson_authored_event_counts_as_taught() {
    let g = graph_of(vec![atom("a", &[])]);
    let p = path_with(&["a"]);
    let events = vec![lesson_authored("a")];
    assert_eq!(
        upcoming_atoms(&g, &p.targets, &common::progress_of(&events)),
        Vec::<String>::new()
    );
}

#[test]
fn prereq_appears_before_target() {
    let g = graph_of(vec![atom("pre", &[]), atom("target", &["pre"])]);
    let p = path_with(&["target"]);
    assert_eq!(
        upcoming_atoms(&g, &p.targets, &PathProgress::default()),
        vec!["pre".to_string(), "target".to_string()],
    );
}

#[test]
fn taught_prereq_is_skipped_but_target_remains() {
    let g = graph_of(vec![atom("pre", &[]), atom("target", &["pre"])]);
    let p = path_with(&["target"]);
    let events = vec![taught("pre")];
    assert_eq!(
        upcoming_atoms(&g, &p.targets, &common::progress_of(&events)),
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
    let order = upcoming_atoms(&g, &p.targets, &PathProgress::default());
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
    let order = upcoming_atoms(&g, &p.targets, &PathProgress::default());
    assert!(!order.contains(&"la.2".to_string()));
    assert!(order.contains(&"la.2.1".to_string()));
    assert!(order.contains(&"la.2.2".to_string()));
}

#[test]
fn multiple_targets_walked_in_order() {
    let g = graph_of(vec![atom("a", &[]), atom("b", &[])]);
    let p = path_with(&["a", "b"]);
    assert_eq!(
        upcoming_atoms(&g, &p.targets, &PathProgress::default()),
        vec!["a".to_string(), "b".to_string()],
    );
}

#[test]
fn missing_id_is_silently_skipped() {
    let g = graph_of(vec![atom("a", &[])]);
    let p = path_with(&["a", "ghost"]);
    assert_eq!(
        upcoming_atoms(&g, &p.targets, &PathProgress::default()),
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
    let upcoming = upcoming_atoms(&g, &p.targets, &PathProgress::default());
    assert_eq!(upcoming.len(), 4);
    let n = 2;
    let truncated: Vec<_> = upcoming.iter().take(n).cloned().collect();
    assert_eq!(truncated, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn top_down_lists_targets_not_prereqs() {
    let g = graph_of(vec![atom("p", &[]), atom("t", &["p"])]);
    let p = path_with_strategy(&["t"], Strategy::TopDown);
    assert_eq!(
        upcoming_top_down(&g, &p.targets, &PathProgress::default(), &[]),
        vec!["t".to_string()],
    );
}

#[test]
fn top_down_prefixes_subpath_before_targets() {
    let g = graph_of(vec![atom("p", &[]), atom("q", &[]), atom("t", &["p", "q"])]);
    let p = path_with_strategy(&["t"], Strategy::TopDown);
    let subpath = vec!["q".to_string(), "t".to_string()];
    assert_eq!(
        upcoming_top_down(&g, &p.targets, &PathProgress::default(), &subpath),
        vec!["q".to_string(), "t".to_string()],
    );
}

#[test]
fn top_down_skips_taught_subpath_and_target_atoms() {
    let g = graph_of(vec![atom("p", &[]), atom("t", &["p"])]);
    let p = path_with_strategy(&["t"], Strategy::TopDown);
    let subpath = vec!["p".to_string(), "t".to_string()];
    assert_eq!(
        upcoming_top_down(
            &g,
            &p.targets,
            &common::progress_of(&[taught("p")]),
            &subpath
        ),
        vec!["t".to_string()],
    );
}

/// Run `mt path next` and act on each action until the path is done, and return the atoms in the order that their lessons were taught.
async fn teach_until_done(db: &Db, path_id: &str) -> Vec<String> {
    let mut taught = Vec::new();
    for _ in 0..100 {
        let envelope = scheduler::compute_next(db, Some(path_id), NextMode::Default, Surface::Cli)
            .await
            .unwrap();
        match envelope.action().clone() {
            Action::CreateLesson { atom_id } => {
                store::cmd_lesson_upsert(db, &atom_id, "body".into(), Some(path_id))
                    .await
                    .unwrap();
                taught.push(atom_id);
            }
            Action::PresentLesson { atom_id } => taught.push(atom_id),
            Action::CreateQuiz {
                atom_id,
                difficulty,
            } => {
                let draft = QuizDraft {
                    difficulty,
                    question: "q".into(),
                    answer: "a".into(),
                    rubric: None,
                    kind: QuizType::FreeText,
                };
                store::cmd_quiz_create(db, &atom_id, draft, Some(path_id))
                    .await
                    .unwrap();
            }
            Action::PresentQuiz { quiz_id, .. } => {
                answer::cmd_quiz_answer(db, &quiz_id, Rating::Good, None, Some(path_id))
                    .await
                    .unwrap();
            }
            Action::NoReviewsDue | Action::Done => return taught,
        }
    }
    panic!("path {path_id} did not finish");
}

#[tokio::test]
async fn syllabus_lists_atoms_in_the_order_that_next_teaches_them() {
    let dir = common::write_graph(
        "
  - id: ta.1
    name: prerequisites
    children:
      - id: ta.1.1
        name: base
        description: d
      - id: ta.1.2
        name: second
        description: d
        prerequisites: [ta.1.1]
      - id: ta.1.3
        name: third
        description: d
        prerequisites: [ta.1.1]
  - id: ta.2
    name: goals
    children:
      - id: ta.2.1
        name: target
        description: d
        prerequisites: [ta.1.3, ta.1.2]
      - id: ta.2.2
        name: other target
        description: d
        prerequisites: [ta.1.2]
",
    );
    let cfg = DbConfig {
        curriculum: Curriculum::open(Some(dir.path().into())),
        ..common::fresh_config()
    };
    let db = common::create_db(&cfg).await;
    let targets = ["ta.2".to_string()];
    for strategy in [Strategy::BottomUp, Strategy::TopDown] {
        let id = path::cmd_path_new(&db, "order", &targets, strategy)
            .await
            .unwrap();
        if strategy == Strategy::TopDown {
            let atoms = ["ta.1.2".to_string(), "ta.2.2".to_string()];
            subpath::cmd_subpath_set(&db, Some(&id), &atoms)
                .await
                .unwrap();
        }
        let syllabus = syllabus::compute_syllabus(&db, Some(&id), 100)
            .await
            .unwrap();
        let listed: Vec<String> = syllabus.atoms.into_iter().map(|a| a.id).collect();
        assert!(listed.len() > 2, "{strategy}: {listed:?}");
        assert_eq!(teach_until_done(&db, &id).await, listed, "{strategy}");
    }
}
