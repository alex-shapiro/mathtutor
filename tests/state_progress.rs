//! Unit and integration tests for `PathCompletion::of` and `state::compute_state`.

use chrono::{Duration, Utc};
use mathtutor::cards::{CardRow, CardState};
use mathtutor::graph::Graph;
use mathtutor::overlay::QuizDraft;
use mathtutor::path::PathFile;
use mathtutor::progress::{PathCompletion, PathProgress, ReachProgress, TargetProgress};
use mathtutor::state;

mod common;

use common::{
    PATH_ID, complete_atom, complete_events, empty_atom, graph_of, path_with, progress_of, taught,
};

fn completion(g: &Graph, p: &PathFile, progress: &PathProgress) -> (TargetProgress, ReachProgress) {
    let c = PathCompletion::of(g, &p.resolve_targets(g).unwrap(), progress);
    (c.targets, c.reachable)
}

#[test]
fn target_complete_counts_toward_both_targets_and_reachable() {
    let g = graph_of(vec![complete_atom("a", &[])]);
    let p = path_with(&["a"]);
    let events = complete_events("a");

    let (t, r) = completion(&g, &p, &progress_of(&events));
    assert_eq!(t.total, 1);
    assert_eq!(t.learned, 1);
    assert_eq!(t.learned_pct, 100);
    assert_eq!(r.total, 1);
    assert_eq!(r.taught, 1);
    assert_eq!(r.learned, 1);
}

#[test]
fn prereq_complete_counts_toward_reachable_only() {
    let g = graph_of(vec![
        complete_atom("pre", &[]),
        complete_atom("target", &["pre"]),
    ]);
    let p = path_with(&["target"]);
    let events = complete_events("pre");

    let (t, r) = completion(&g, &p, &progress_of(&events));
    assert_eq!(t.total, 1);
    assert_eq!(t.learned, 0, "target itself is not yet complete");
    assert_eq!(t.learned_pct, 0);
    assert_eq!(r.total, 2);
    assert_eq!(r.learned, 1, "prereq counts as a reachable learned atom");
}

#[test]
fn taught_counts_lesson_taught_in_path_regardless_of_quiz_progress() {
    let g = graph_of(vec![complete_atom("a", &[])]);
    let p = path_with(&["a"]);
    let events = vec![taught("a")];

    let (t, r) = completion(&g, &p, &progress_of(&events));
    assert_eq!(t.learned, 0);
    assert_eq!(r.taught, 1);
    assert_eq!(r.learned, 0);
}

#[test]
fn empty_path_yields_zero_progress_without_division_panic() {
    let g = graph_of(vec![]);
    let p = path_with(&[]);

    let (t, r) = completion(&g, &p, &PathProgress::default());
    assert_eq!(t.total, 0);
    assert_eq!(t.learned, 0);
    assert_eq!(t.learned_pct, 0, "zero targets must not divide by zero");
    assert_eq!(r.total, 0);
    assert_eq!(r.taught, 0);
    assert_eq!(r.learned, 0);
}

#[test]
fn reachable_includes_transitive_prereqs() {
    let g = graph_of(vec![
        empty_atom("a", &[]),
        empty_atom("b", &["a"]),
        empty_atom("c", &["b"]),
    ]);
    let p = path_with(&["c"]);

    let (_t, r) = completion(&g, &p, &PathProgress::default());
    assert_eq!(r.total, 3);
}

#[test]
fn learned_pct_rounds_down() {
    let g = graph_of(vec![
        complete_atom("a", &[]),
        complete_atom("b", &[]),
        complete_atom("c", &[]),
    ]);
    let p = path_with(&["a", "b", "c"]);
    let events = complete_events("a");

    let (t, _r) = completion(&g, &p, &progress_of(&events));
    assert_eq!(t.learned, 1);
    assert_eq!(t.learned_pct, 33);
}

async fn seed_card(
    db: &mathtutor::db::Db,
    quiz_id: &str,
    due: chrono::DateTime<Utc>,
    reviewed: chrono::DateTime<Utc>,
) {
    let card = CardRow {
        state: Some(CardState {
            due,
            last_review: reviewed,
            stability: 1.0,
            difficulty: 5.0,
        }),
        reps: 1,
        ..CardRow::new(PATH_ID, quiz_id)
    };
    common::write(db, PATH_ID, |_, changes| {
        changes.set_card(card.clone());
    })
    .await;
}

#[tokio::test]
async fn compute_state_past_due_is_zero_on_fresh_path() {
    let db = common::fresh_db(PATH_ID).await;

    let s = state::compute_state(&db, Some(PATH_ID))
        .await
        .expect("compute_state");
    assert_eq!(s.past_due, 0);
}

#[tokio::test]
async fn compute_state_past_due_excludes_future_cards() {
    let db = common::fresh_db(PATH_ID).await;
    let now = Utc::now();

    for (quiz_id, offset_secs) in [("q.a", 60_i64), ("q.b", 3600), ("q.c", 86_400)] {
        seed_card(&db, quiz_id, now + Duration::seconds(offset_secs), now).await;
    }

    let s = state::compute_state(&db, Some(PATH_ID))
        .await
        .expect("compute_state");
    assert_eq!(s.past_due, 0);
}

#[tokio::test]
async fn compute_state_past_due_counts_overdue_cards() {
    let db = common::fresh_db(PATH_ID).await;
    let now = Utc::now();

    mathtutor::store::cmd_lesson_upsert(&db, "fnd.1.1.1", "body".into(), Some(PATH_ID))
        .await
        .unwrap();
    for offset_secs in [-3600_i64, -60, 3600] {
        let quiz_id = mathtutor::store::cmd_quiz_create(
            &db,
            "fnd.1.1.1",
            QuizDraft {
                difficulty: mathtutor::types::Difficulty::Easy,
                question: "q".into(),
                answer: "a".into(),
                rubric: None,
                kind: mathtutor::types::QuizType::FreeText,
            },
            Some(PATH_ID),
        )
        .await
        .unwrap();
        let due = now + Duration::seconds(offset_secs);
        seed_card(&db, &quiz_id, due, due - Duration::seconds(1)).await;
    }

    let s = state::compute_state(&db, Some(PATH_ID))
        .await
        .expect("compute_state");
    assert_eq!(s.past_due, 2);
}

#[test]
fn write_state_emits_past_due_line() {
    let mut buf = Vec::new();
    let s = state::StateSummary {
        path: PATH_ID.into(),
        goal: "test".into(),
        strategy: mathtutor::types::Strategy::BottomUp,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        targets: TargetProgress {
            total: 0,
            learned: 0,
            learned_pct: 0,
        },
        reachable: Some(ReachProgress {
            total: 0,
            taught: 0,
            learned: 0,
        }),
        subpath: Vec::new(),
        past_due: 7,
        most_recent: None,
        next: None,
    };
    state::write_state(&mut buf, &s).expect("write_state");
    let out = String::from_utf8(buf).unwrap();
    assert!(
        out.lines()
            .any(|l| l.starts_with("past due:") && l.contains('7')),
        "missing 'past due: 7' line in:\n{out}"
    );
}

#[tokio::test]
async fn past_due_ignores_a_deleted_quiz() {
    let db = common::empty_db().await;
    common::seed_path(
        &db,
        common::PATH_ID,
        &["fnd.1.1.1"],
        mathtutor::types::Strategy::BottomUp,
    )
    .await;
    mathtutor::store::cmd_lesson_upsert(&db, "fnd.1.1.1", "body".into(), Some(common::PATH_ID))
        .await
        .unwrap();
    let quiz_id = mathtutor::store::cmd_quiz_create(
        &db,
        "fnd.1.1.1",
        QuizDraft {
            difficulty: mathtutor::types::Difficulty::Easy,
            question: "q".into(),
            answer: "a".into(),
            rubric: None,
            kind: mathtutor::types::QuizType::FreeText,
        },
        Some(common::PATH_ID),
    )
    .await
    .unwrap();
    let long_ago = Utc::now() - Duration::days(365);
    common::append(
        &db,
        &[common::at(
            long_ago,
            common::answered(&quiz_id, mathtutor::types::Rating::Good),
        )],
    )
    .await;
    let past_due = async || {
        state::compute_state(&db, Some(common::PATH_ID))
            .await
            .unwrap()
            .past_due
    };
    assert_eq!(past_due().await, 1);

    mathtutor::store::cmd_quiz_delete(&db, &quiz_id, Some(common::PATH_ID))
        .await
        .unwrap();

    assert_eq!(past_due().await, 0);
}
