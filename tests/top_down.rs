//! Top-down strategy + subpath: scheduler branch, strategy persistence,
//! and subpath storage / validation.

mod common;

use common::{PATH_ID, cluster, complete_atom, empty_atom, fresh_db, graph_of, path_with_strategy};

use chrono::Utc;
use libsql::Connection;
use mathtutor::db::{self, DbConfig};
use mathtutor::path::{self, PathFile};
use mathtutor::progress::PathProgress;
use mathtutor::scheduler::{self, Action, NextMode};
use mathtutor::types::Strategy;
use mathtutor::{Error, subpath};
use tempfile::TempDir;

const NO_DUE: &[(String, chrono::DateTime<Utc>)] = &[];

/// A `PathProgress` marking each id complete: lesson taught and all three
/// derived quizzes (`{id}.q1/.q2/.q3`, per `complete_atom`) answered.
fn progress_complete(ids: &[&str]) -> PathProgress {
    let mut p = PathProgress::default();
    for id in ids {
        p.taught_atoms.insert((*id).to_string());
        for n in 1..=3 {
            p.correct_quizzes.insert(format!("{id}.q{n}"));
        }
    }
    p
}

fn atom_id(a: &Action) -> Option<&str> {
    a.atom_id()
}

// ── scheduler branch ────────────────────────────────────────────────

#[test]
fn top_down_presents_target_before_its_prereqs() {
    // target `t` depends on `p`; nothing taught yet.
    let g = graph_of(vec![empty_atom("p", &[]), empty_atom("t", &["p"])]);
    let p = path_with_strategy(&["t"], Strategy::TopDown);
    let action = scheduler::next_action(
        &g,
        &p,
        &p.targets,
        &PathProgress::default(),
        NO_DUE,
        &[],
        NextMode::Default,
    );
    assert_eq!(
        atom_id(&action),
        Some("t"),
        "top-down teaches the target first, not its prerequisite",
    );
}

#[test]
fn bottom_up_descends_into_prereqs_first() {
    // Same graph, contrasting strategy: bottom-up reaches the deepest
    // unlearned prerequisite before the target.
    let g = graph_of(vec![empty_atom("p", &[]), empty_atom("t", &["p"])]);
    let p = path_with_strategy(&["t"], Strategy::BottomUp);
    let action = scheduler::next_action(
        &g,
        &p,
        &p.targets,
        &PathProgress::default(),
        NO_DUE,
        &[],
        NextMode::Default,
    );
    assert_eq!(
        atom_id(&action),
        Some("p"),
        "bottom-up teaches prereqs first"
    );
}

#[test]
fn top_down_subpath_walked_in_order() {
    // With a subpath set, `next` serves its first incomplete atom even
    // though the bare target would otherwise come first.
    let g = graph_of(vec![empty_atom("p", &[]), empty_atom("t", &["p"])]);
    let p = path_with_strategy(&["t"], Strategy::TopDown);
    let subpath = vec!["p".to_string(), "t".to_string()];
    let action = scheduler::next_action(
        &g,
        &p,
        &p.targets,
        &PathProgress::default(),
        NO_DUE,
        &subpath,
        NextMode::Default,
    );
    assert_eq!(atom_id(&action), Some("p"), "first incomplete subpath atom");
}

#[test]
fn top_down_subpath_advances_when_earlier_atom_complete() {
    let g = graph_of(vec![complete_atom("p", &[]), empty_atom("t", &["p"])]);
    let p = path_with_strategy(&["t"], Strategy::TopDown);
    let subpath = vec!["p".to_string(), "t".to_string()];
    let progress = progress_complete(&["p"]);
    let action = scheduler::next_action(
        &g,
        &p,
        &p.targets,
        &progress,
        NO_DUE,
        &subpath,
        NextMode::Default,
    );
    assert_eq!(
        atom_id(&action),
        Some("t"),
        "once `p` is complete the subpath advances to the target",
    );
}

#[test]
fn top_down_drained_subpath_falls_through_to_targets() {
    // Subpath holds only the (now complete) prereq; `next` falls through
    // to the still-incomplete target.
    let g = graph_of(vec![complete_atom("p", &[]), empty_atom("t", &["p"])]);
    let p = path_with_strategy(&["t"], Strategy::TopDown);
    let subpath = vec!["p".to_string()];
    let progress = progress_complete(&["p"]);
    let action = scheduler::next_action(
        &g,
        &p,
        &p.targets,
        &progress,
        NO_DUE,
        &subpath,
        NextMode::Default,
    );
    assert_eq!(
        atom_id(&action),
        Some("t"),
        "fall through to remaining targets"
    );
}

#[test]
fn top_down_done_when_targets_complete() {
    let g = graph_of(vec![complete_atom("t", &[])]);
    let p = path_with_strategy(&["t"], Strategy::TopDown);
    let progress = progress_complete(&["t"]);
    let action = scheduler::next_action(
        &g,
        &p,
        &p.targets,
        &progress,
        NO_DUE,
        &[],
        NextMode::Default,
    );
    assert!(
        matches!(action, Action::Done),
        "all targets complete → done"
    );
}

// ── cluster targets resolve on load ─────────────────────────────────

#[test]
fn top_down_cluster_target_resolves_to_atomic_children() {
    // Regression: a target that's a cluster (e.g. an atom the curriculum
    // later split into one) must expand to its atoms rather than be
    // skipped or treated as a lesson-less atom. `resolve_targets` does the
    // expansion; the scheduler then descends into the first child.
    let g = graph_of(vec![
        cluster("tx.1", &["tx.1.1", "tx.1.2"]),
        empty_atom("tx.1.1", &[]),
        empty_atom("tx.1.2", &[]),
    ]);
    let p = path_with_strategy(&["tx.1"], Strategy::TopDown);

    let targets = p.resolve_targets(&g).unwrap();
    assert_eq!(targets, vec!["tx.1.1".to_string(), "tx.1.2".to_string()]);

    let action = scheduler::next_action(
        &g,
        &p,
        &targets,
        &PathProgress::default(),
        NO_DUE,
        &[],
        NextMode::Default,
    );
    assert_eq!(
        atom_id(&action),
        Some("tx.1.1"),
        "top-down descends a cluster target to its first atomic child",
    );
}

#[tokio::test]
async fn cluster_target_stored_verbatim_and_syllabus_not_empty() {
    // End-to-end against the embedded curriculum: a path targeting an area
    // root stores the root verbatim, and `syllabus` expands it to the
    // area's upcoming atoms. Before targets were expanded on load, a
    // top-down cluster target produced an empty syllabus.
    let tmp = TempDir::new().unwrap();
    let conn = open_db(&tmp).await;
    let id = path::cmd_path_new(
        &conn,
        "transformers",
        &["tx".into()],
        Strategy::TopDown,
        None,
    )
    .await
    .expect("new path");

    assert_eq!(
        path::load_path(&conn, &id).await.unwrap().targets,
        vec!["tx".to_string()],
        "the area root is stored as given, not pre-expanded",
    );

    let view = mathtutor::syllabus::compute_syllabus(&conn, Some(&id), 10, None)
        .await
        .expect("syllabus");
    assert!(
        view.total_remaining > 0 && !view.atoms.is_empty(),
        "cluster target must expand to upcoming atoms, got {view:?}",
    );
}

#[tokio::test]
async fn subpath_cluster_atom_resolves_on_load() {
    // Regression: a subpath atom that a later curriculum edit split into a
    // cluster must expand on load — same as targets — rather than being
    // served to the scheduler as a lesson-less cluster. Set-time validation
    // forbids cluster atoms, so we store one directly to simulate the drift.
    let tmp = TempDir::new().unwrap();
    let conn = open_db(&tmp).await;
    let id = save(&conn, &["tx.1.1.1"], Strategy::TopDown).await;
    subpath::replace(&conn, &id, &["tx.1".into()])
        .await
        .unwrap();

    let g = mathtutor::graph::Graph::load_for_path(&conn, None)
        .await
        .unwrap();
    let resolved = subpath::load_resolved(&conn, &id, &g).await.unwrap();

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

// ── strategy persistence ────────────────────────────────────────────

async fn open_db(dir: &TempDir) -> Connection {
    let cfg = DbConfig::local(dir.path().join("mt.db"));
    let database = db::open(&cfg).await.expect("open");
    db::connect(&database).await.expect("connect")
}

async fn save(conn: &Connection, targets: &[&str], strategy: Strategy) -> String {
    let p = PathFile {
        id: PATH_ID.into(),
        goal: "test".into(),
        created_at: Utc::now(),
        targets: targets.iter().map(|s| (*s).to_string()).collect(),
        strategy,
    };
    path::save_path(conn, &p).await.expect("save_path");
    p.id
}

#[tokio::test]
async fn strategy_persists_and_switches() {
    let tmp = TempDir::new().unwrap();
    let conn = open_db(&tmp).await;
    let id = save(&conn, &["fnd.1.1.2"], Strategy::TopDown).await;

    assert_eq!(
        path::load_path(&conn, &id).await.unwrap().strategy,
        Strategy::TopDown,
    );

    path::cmd_path_strategy(&conn, Some(&id), Strategy::BottomUp)
        .await
        .unwrap();
    assert_eq!(
        path::load_path(&conn, &id).await.unwrap().strategy,
        Strategy::BottomUp,
        "the switch is persisted",
    );
}

#[tokio::test]
async fn fresh_db_path_defaults_to_bottom_up() {
    // A row inserted without the strategy column (as pre-feature paths
    // were) takes the migration default.
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp, PATH_ID).await;
    assert_eq!(
        path::load_path(&conn, PATH_ID).await.unwrap().strategy,
        Strategy::BottomUp,
    );
}

// ── subpath storage ─────────────────────────────────────────────────

#[tokio::test]
async fn subpath_round_trips_and_clears() {
    let tmp = TempDir::new().unwrap();
    let conn = open_db(&tmp).await;
    let id = save(&conn, &["fnd.1.1.2"], Strategy::TopDown).await;

    subpath::replace(&conn, &id, &["a".into(), "b".into(), "c".into()])
        .await
        .unwrap();
    assert_eq!(
        subpath::load(&conn, &id).await.unwrap(),
        vec!["a", "b", "c"]
    );

    // Replacement is wholesale, not additive.
    subpath::replace(&conn, &id, &["x".into(), "y".into()])
        .await
        .unwrap();
    assert_eq!(subpath::load(&conn, &id).await.unwrap(), vec!["x", "y"]);

    subpath::clear(&conn, &id).await.unwrap();
    assert!(subpath::load(&conn, &id).await.unwrap().is_empty());
}

// ── subpath command validation ──────────────────────────────────────

#[tokio::test]
async fn subpath_set_happy_path() {
    let tmp = TempDir::new().unwrap();
    let conn = open_db(&tmp).await;
    let id = save(&conn, &["fnd.1.1.2"], Strategy::TopDown).await;

    subpath::cmd_subpath_set(
        &conn,
        Some(&id),
        &["fnd.1.1.1".into(), "fnd.1.1.2".into()],
        None,
    )
    .await
    .expect("valid subpath ending in a target");
    assert_eq!(
        subpath::load(&conn, &id).await.unwrap(),
        vec!["fnd.1.1.1", "fnd.1.1.2"],
    );
}

#[tokio::test]
async fn subpath_set_rejected_on_bottom_up_path() {
    let tmp = TempDir::new().unwrap();
    let conn = open_db(&tmp).await;
    let id = save(&conn, &["fnd.1.1.2"], Strategy::BottomUp).await;

    let err = subpath::cmd_subpath_set(&conn, Some(&id), &["fnd.1.1.2".into()], None)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::SubpathNotTopDown), "got {err:?}");
}

#[tokio::test]
async fn subpath_set_requires_target_tail() {
    let tmp = TempDir::new().unwrap();
    let conn = open_db(&tmp).await;
    let id = save(&conn, &["fnd.1.1.2"], Strategy::TopDown).await;

    let err = subpath::cmd_subpath_set(&conn, Some(&id), &["fnd.1.1.1".into()], None)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::SubpathTailNotTarget(_)), "got {err:?}");
}

#[tokio::test]
async fn subpath_set_rejects_empty_and_duplicates_and_unknown() {
    let tmp = TempDir::new().unwrap();
    let conn = open_db(&tmp).await;
    let id = save(&conn, &["fnd.1.1.2"], Strategy::TopDown).await;

    let empty = subpath::cmd_subpath_set(&conn, Some(&id), &[], None)
        .await
        .unwrap_err();
    assert!(matches!(empty, Error::SubpathEmpty), "got {empty:?}");

    let dup = subpath::cmd_subpath_set(
        &conn,
        Some(&id),
        &["fnd.1.1.2".into(), "fnd.1.1.2".into()],
        None,
    )
    .await
    .unwrap_err();
    assert!(matches!(dup, Error::SubpathDuplicateAtom(_)), "got {dup:?}");

    let unknown = subpath::cmd_subpath_set(
        &conn,
        Some(&id),
        &["nope.0.0.0".into(), "fnd.1.1.2".into()],
        None,
    )
    .await
    .unwrap_err();
    assert!(matches!(unknown, Error::AtomNotFound(_)), "got {unknown:?}");
}
