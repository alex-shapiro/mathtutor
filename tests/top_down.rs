//! Unit and integration tests for top-down scheduling, strategy persistence, and subpaths.

mod common;

use common::{
    PATH_ID, cluster, complete_atom, complete_events, empty_atom, path_state, path_with_strategy,
};

use mathtutor::db::Db;
use mathtutor::graph::Graph;
use mathtutor::path::{self, PathFile};
use mathtutor::scheduler::{Action, NextMode};
use mathtutor::types::Strategy;
use mathtutor::{Error, subpath};

fn atom_id(a: &Action) -> Option<&str> {
    a.atom_id()
}

#[test]
fn top_down_presents_target_before_its_prereqs() {
    let g = Graph::of_concepts(vec![empty_atom("p", &[]), empty_atom("t", &["p"])]);
    let p = path_with_strategy(&["t"], Strategy::TopDown);
    let action = common::next_action(&g, &p, &[], NextMode::Default);
    assert_eq!(
        atom_id(&action),
        Some("t"),
        "top-down teaches the target first, not its prerequisite",
    );
}

#[test]
fn bottom_up_descends_into_prereqs_first() {
    let g = Graph::of_concepts(vec![empty_atom("p", &[]), empty_atom("t", &["p"])]);
    let p = path_with_strategy(&["t"], Strategy::BottomUp);
    let action = common::next_action(&g, &p, &[], NextMode::Default);
    assert_eq!(
        atom_id(&action),
        Some("p"),
        "bottom-up teaches prereqs first"
    );
}

#[test]
fn top_down_subpath_walked_in_order() {
    let g = Graph::of_concepts(vec![empty_atom("p", &[]), empty_atom("t", &["p"])]);
    let mut p = path_with_strategy(&["t"], Strategy::TopDown);
    p.subpath = vec!["p".to_string(), "t".to_string()];
    let action = common::next_action(&g, &p, &[], NextMode::Default);
    assert_eq!(atom_id(&action), Some("p"), "first incomplete subpath atom");
}

#[test]
fn top_down_subpath_advances_once_earlier_atom_completes() {
    let g = Graph::of_concepts(vec![complete_atom("p", &[]), empty_atom("t", &["p"])]);
    let mut p = path_with_strategy(&["t"], Strategy::TopDown);
    p.subpath = vec!["p".to_string(), "t".to_string()];
    let events = complete_events("p");
    let action = common::next_action(&g, &p, &events, NextMode::Default);
    assert_eq!(
        atom_id(&action),
        Some("t"),
        "once `p` is complete the subpath advances to the target",
    );
}

#[test]
fn top_down_drained_subpath_falls_through_to_targets() {
    let g = Graph::of_concepts(vec![complete_atom("p", &[]), empty_atom("t", &["p"])]);
    let mut p = path_with_strategy(&["t"], Strategy::TopDown);
    p.subpath = vec!["p".to_string()];
    let events = complete_events("p");
    let action = common::next_action(&g, &p, &events, NextMode::Default);
    assert_eq!(
        atom_id(&action),
        Some("t"),
        "fall through to remaining targets"
    );
}

#[test]
fn top_down_returns_done_if_targets_complete() {
    let g = Graph::of_concepts(vec![complete_atom("t", &[])]);
    let p = path_with_strategy(&["t"], Strategy::TopDown);
    let events = complete_events("t");
    let action = common::next_action(&g, &p, &events, NextMode::Default);
    assert!(
        matches!(action, Action::Done),
        "all targets complete → done"
    );
}

#[test]
fn top_down_cluster_target_expands_to_atomic_children() {
    let g = Graph::of_concepts(vec![
        cluster("tx.1", &["tx.1.1", "tx.1.2"]),
        empty_atom("tx.1.1", &[]),
        empty_atom("tx.1.2", &[]),
    ]);
    let p = path_with_strategy(&["tx.1"], Strategy::TopDown);

    let action = common::next_action(&g, &p, &[], NextMode::Default);
    assert_eq!(
        atom_id(&action),
        Some("tx.1.1"),
        "top-down descends a cluster target to its first atomic child",
    );
}

#[test]
fn top_down_teaches_the_atoms_of_a_prerequisite_cluster_first() {
    let g = Graph::of_concepts(vec![
        cluster("tx", &["tx.1", "tx.2"]),
        cluster("tx.1", &["tx.1.1"]),
        cluster("tx.2", &["tx.2.1"]),
        empty_atom("tx.1.1", &["tx.2"]),
        empty_atom("tx.2.1", &[]),
    ]);
    let p = path_with_strategy(&["tx"], Strategy::TopDown);
    assert_eq!(
        common::upcoming(&g, &p, &[]),
        vec!["tx.2.1", "tx.1.1"],
        "`tx.1.1` needs cluster `tx.2`, so the atoms of `tx.2` come first",
    );
}

#[test]
fn top_down_teaches_the_prerequisites_of_an_ancestor_cluster_first() {
    let mut holder = cluster("tx.1", &["tx.1.1"]);
    holder.prerequisites = vec!["tx.2".to_string()];
    let g = Graph::of_concepts(vec![
        cluster("tx", &["tx.1", "tx.2"]),
        holder,
        cluster("tx.2", &["tx.2.1"]),
        empty_atom("tx.1.1", &[]),
        empty_atom("tx.2.1", &[]),
    ]);
    let top_down = path_with_strategy(&["tx"], Strategy::TopDown);
    assert_eq!(
        common::upcoming(&g, &top_down, &[]),
        vec!["tx.2.1", "tx.1.1"]
    );
    let bottom_up = path_with_strategy(&["tx.1.1"], Strategy::BottomUp);
    assert_eq!(
        common::upcoming(&g, &bottom_up, &[]),
        vec!["tx.2.1", "tx.1.1"],
        "bottom-up reaches the prerequisite of the cluster that holds the target",
    );
}

#[tokio::test]
async fn cluster_target_stored_verbatim_and_syllabus_not_empty() {
    let conn = common::empty_db().await;
    let id = path::cmd_path_new(&conn, "transformers", &["tx".into()], Strategy::TopDown)
        .await
        .expect("new path");

    assert_eq!(
        load_path(&conn, &id).await.targets,
        vec!["tx".to_string()],
        "the area root is stored as given, not pre-expanded",
    );

    let view = mathtutor::syllabus::compute_syllabus(&conn, Some(&id), 10)
        .await
        .expect("syllabus");
    assert!(
        view.total_remaining > 0 && !view.atoms.is_empty(),
        "cluster target must expand to upcoming atoms, got {view:?}",
    );
}

#[tokio::test]
async fn subpath_cluster_atom_expands_on_load() {
    // Store a cluster ID directly to simulate curriculum refactoring after subpath creation.
    let conn = common::empty_db().await;
    let id = save(&conn, &["tx.1.1.1"], Strategy::TopDown).await;
    replace_subpath(&conn, &id, &["tx.1".into()]).await;

    let g = mathtutor::graph::Curriculum::embedded()
        .merged(&common::overlay(&conn).await)
        .unwrap();
    let resolved = subpath::resolved(&load_path(&conn, &id).await, &g).unwrap();

    assert!(
        !resolved.contains(&"tx.1".to_string()),
        "the cluster id itself drops out",
    );
    assert!(
        resolved.contains(&"tx.1.1.1".to_string()),
        "the cluster expands to its atomic descendants",
    );
    assert!(
        resolved.iter().all(|a| g.by_id[a].children_ids.is_empty()),
        "every resolved subpath entry is a leaf atom",
    );
}

async fn save(db: &Db, targets: &[&str], strategy: Strategy) -> String {
    common::seed_path(db, PATH_ID, targets, strategy).await;
    PATH_ID.into()
}

async fn load_path(db: &Db, id: &str) -> PathFile {
    path_state(db, id).await.file
}

async fn replace_subpath(db: &Db, id: &str, atoms: &[String]) {
    common::write(db, id, |view, changes| {
        let p = PathFile {
            subpath: atoms.to_vec(),
            ..view.path().unwrap().file.clone()
        };
        changes.replace_path(&p);
    })
    .await;
}

#[tokio::test]
async fn strategy_persists_and_switches() {
    let conn = common::empty_db().await;
    let id = save(&conn, &["fnd.1.1.2"], Strategy::TopDown).await;

    assert_eq!(load_path(&conn, &id).await.strategy, Strategy::TopDown,);

    path::cmd_path_strategy(&conn, Some(&id), Strategy::BottomUp)
        .await
        .unwrap();
    assert_eq!(
        load_path(&conn, &id).await.strategy,
        Strategy::BottomUp,
        "the switch is persisted",
    );
}

#[tokio::test]
async fn subpath_round_trips_and_clears() {
    let conn = common::empty_db().await;
    let id = save(&conn, &["fnd.1.1.2"], Strategy::TopDown).await;

    replace_subpath(&conn, &id, &["a".into(), "b".into(), "c".into()]).await;
    assert_eq!(load_path(&conn, &id).await.subpath, vec!["a", "b", "c"]);

    replace_subpath(&conn, &id, &["x".into(), "y".into()]).await;
    assert_eq!(load_path(&conn, &id).await.subpath, vec!["x", "y"]);

    subpath::cmd_subpath_clear(&conn, Some(&id)).await.unwrap();
    assert_eq!(load_path(&conn, &id).await.subpath, Vec::<String>::new());
}

#[tokio::test]
async fn subpath_set_happy_path() {
    let conn = common::empty_db().await;
    let id = save(&conn, &["fnd.1.1.2"], Strategy::TopDown).await;

    subpath::cmd_subpath_set(&conn, Some(&id), &["fnd.1.1.1".into(), "fnd.1.1.2".into()])
        .await
        .expect("valid subpath ending in a target");
    assert_eq!(
        load_path(&conn, &id).await.subpath,
        vec!["fnd.1.1.1", "fnd.1.1.2"],
    );
}

#[tokio::test]
async fn subpath_set_rejected_on_bottom_up_path() {
    let conn = common::empty_db().await;
    let id = save(&conn, &["fnd.1.1.2"], Strategy::BottomUp).await;

    let err = subpath::cmd_subpath_set(&conn, Some(&id), &["fnd.1.1.2".into()])
        .await
        .unwrap_err();
    assert!(matches!(err, Error::SubpathNotTopDown), "got {err:?}");
}

#[tokio::test]
async fn subpath_set_requires_target_tail() {
    let conn = common::empty_db().await;
    let id = save(&conn, &["fnd.1.1.2"], Strategy::TopDown).await;

    let err = subpath::cmd_subpath_set(&conn, Some(&id), &["fnd.1.1.1".into()])
        .await
        .unwrap_err();
    assert!(matches!(err, Error::SubpathTailNotTarget(_)), "got {err:?}");
}

#[tokio::test]
async fn subpath_set_rejects_empty_and_duplicates_and_unknown() {
    let conn = common::empty_db().await;
    let id = save(&conn, &["fnd.1.1.2"], Strategy::TopDown).await;

    let empty = subpath::cmd_subpath_set(&conn, Some(&id), &[])
        .await
        .unwrap_err();
    assert!(matches!(empty, Error::SubpathEmpty), "got {empty:?}");

    let dup = subpath::cmd_subpath_set(&conn, Some(&id), &["fnd.1.1.2".into(), "fnd.1.1.2".into()])
        .await
        .unwrap_err();
    assert!(matches!(dup, Error::SubpathDuplicateAtom(_)), "got {dup:?}");

    let unknown =
        subpath::cmd_subpath_set(&conn, Some(&id), &["nope.0.0.0".into(), "fnd.1.1.2".into()])
            .await
            .unwrap_err();
    assert!(matches!(unknown, Error::UnknownId(_)), "got {unknown:?}");
}
