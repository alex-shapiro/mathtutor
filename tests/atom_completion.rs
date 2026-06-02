//! Regression tests for `mt path next`'s per-atom completion walker.
//!
//! Each test builds an in-memory `Graph`, `PathFile`, and event log,
//! then asserts on the `Action` returned by `scheduler::next_action`.
//! No filesystem, no curriculum AYML — just the scheduler's contract.

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use mathtutor::event_log::{Event, EventKind, EventPayload};
use mathtutor::graph::{FlatConcept, Graph, Quiz};
use mathtutor::path::PathFile;
use mathtutor::progress::PathProgress;
use mathtutor::scheduler::{self, Action};
use mathtutor::types::{Difficulty, Rating};

// ── Fixture builders ────────────────────────────────────────────────

const PATH_ID: &str = "p_test";

fn quiz(id: &str, difficulty: Difficulty) -> Quiz {
    Quiz {
        id: id.to_string(),
        difficulty,
        kind: None,
        question: format!("q? {id}"),
        answer: "a".into(),
        rubric: None,
    }
}

fn atom(id: &str, prereqs: &[&str], lesson: Option<&str>, quizzes: Vec<Quiz>) -> FlatConcept {
    FlatConcept {
        id: id.into(),
        name: id.into(),
        description: None,
        prerequisites: prereqs.iter().map(|s| (*s).to_string()).collect(),
        children_ids: Vec::new(),
        lesson: lesson.map(String::from),
        quizzes,
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
        target_atoms: targets.iter().map(|s| (*s).to_string()).collect(),
    }
}

/// Default empty due-quiz list for tests that don't exercise the FSRS
/// scheduling path. `next_action` only inspects this slice for due cards.
const NO_DUE: &[(String, chrono::DateTime<Utc>)] = &[];

fn answered(quiz_id: &str, rating: Rating, ts: DateTime<Utc>) -> Event {
    Event {
        ts,
        kind: EventKind::QuizAnswered,
        path: PATH_ID.into(),
        atom: None,
        quiz: Some(quiz_id.into()),
        payload: EventPayload {
            rating: Some(rating),
            ..Default::default()
        },
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

fn assert_create_lesson(action: &Action, expected_atom: &str) {
    match action {
        Action::CreateLesson { atom_id } => assert_eq!(atom_id, expected_atom),
        other => panic!("expected CreateLesson({expected_atom}), got {other:?}"),
    }
}

fn assert_create_quiz(action: &Action, expected_atom: &str, expected_diff: Difficulty) {
    match action {
        Action::CreateQuiz {
            atom_id,
            difficulty,
        } => {
            assert_eq!(atom_id, expected_atom);
            assert_eq!(*difficulty, expected_diff);
        }
        other => panic!("expected CreateQuiz({expected_atom}, {expected_diff:?}), got {other:?}"),
    }
}

fn assert_present_quiz(action: &Action, expected_atom: &str, expected_quiz: &str) {
    match action {
        Action::PresentQuiz { atom_id, quiz_id } => {
            assert_eq!(atom_id, expected_atom);
            assert_eq!(quiz_id, expected_quiz);
        }
        other => panic!("expected PresentQuiz({expected_atom}, {expected_quiz}), got {other:?}"),
    }
}

fn assert_present_lesson(action: &Action, expected_atom: &str) {
    match action {
        Action::PresentLesson { atom_id } => assert_eq!(atom_id, expected_atom),
        other => panic!("expected PresentLesson({expected_atom}), got {other:?}"),
    }
}

// ── Walker contract ────────────────────────────────────────────────

#[test]
fn create_lesson_when_atom_has_none() {
    let g = graph_of(vec![atom("a", &[], None, vec![])]);
    let p = path_with(&["a"]);
    assert_create_lesson(
        &scheduler::next_action(&g, &p, &PathProgress::default(), NO_DUE),
        "a",
    );
}

#[test]
fn present_lesson_when_stored_but_not_taught_in_path() {
    // Lesson body exists in the graph (perhaps authored under a prior
    // path), but this path has no `LessonTaught` event yet — surface
    // the stored body before any quiz work.
    let g = graph_of(vec![atom("a", &[], Some("body"), vec![])]);
    let p = path_with(&["a"]);
    assert_present_lesson(
        &scheduler::next_action(&g, &p, &PathProgress::default(), NO_DUE),
        "a",
    );
}

#[test]
fn lesson_authored_event_satisfies_taught_check() {
    // Paths created before `LessonTaught` existed only have
    // `LessonAuthored` events. Those must still register as "taught"
    // so the scheduler doesn't re-present every lesson the user
    // already authored in this path.
    let g = graph_of(vec![atom("a", &[], Some("body"), vec![])]);
    let p = path_with(&["a"]);
    let events = vec![Event {
        ts: Utc::now(),
        kind: EventKind::LessonAuthored,
        path: PATH_ID.into(),
        atom: Some("a".into()),
        quiz: None,
        payload: EventPayload::default(),
    }];
    assert_create_quiz(
        &scheduler::next_action(&g, &p, &PathProgress::from_events(&events), NO_DUE),
        "a",
        Difficulty::Easy,
    );
}

#[test]
fn create_easy_quiz_when_lesson_stored_and_taught() {
    let g = graph_of(vec![atom("a", &[], Some("body"), vec![])]);
    let p = path_with(&["a"]);
    let events = vec![taught("a")];
    assert_create_quiz(
        &scheduler::next_action(&g, &p, &PathProgress::from_events(&events), NO_DUE),
        "a",
        Difficulty::Easy,
    );
}

#[test]
fn present_easy_quiz_when_authored_but_unanswered() {
    let g = graph_of(vec![atom(
        "a",
        &[],
        Some("body"),
        vec![quiz("a.q1", Difficulty::Easy)],
    )]);
    let p = path_with(&["a"]);
    let events = vec![taught("a")];
    assert_present_quiz(
        &scheduler::next_action(&g, &p, &PathProgress::from_events(&events), NO_DUE),
        "a",
        "a.q1",
    );
}

#[test]
fn keep_presenting_easy_after_again_rating() {
    let g = graph_of(vec![atom(
        "a",
        &[],
        Some("body"),
        vec![quiz("a.q1", Difficulty::Easy)],
    )]);
    let p = path_with(&["a"]);
    let events = vec![taught("a"), answered("a.q1", Rating::Again, Utc::now())];
    assert_present_quiz(
        &scheduler::next_action(&g, &p, &PathProgress::from_events(&events), NO_DUE),
        "a",
        "a.q1",
    );
}

#[test]
fn advance_to_medium_after_hard_rating() {
    // `hard` counts as a correct answer (the user got it right, just
    // with effort) — FSRS handles re-presentation timing for hard, so
    // the per-atom walker advances rather than re-surfacing the easy
    // quiz immediately. Only `Again` triggers immediate re-presentation.
    let g = graph_of(vec![atom(
        "a",
        &[],
        Some("body"),
        vec![quiz("a.q1", Difficulty::Easy)],
    )]);
    let p = path_with(&["a"]);
    let events = vec![taught("a"), answered("a.q1", Rating::Hard, Utc::now())];
    assert_create_quiz(
        &scheduler::next_action(&g, &p, &PathProgress::from_events(&events), NO_DUE),
        "a",
        Difficulty::Medium,
    );
}

#[test]
fn advance_to_medium_after_easy_correct() {
    let g = graph_of(vec![atom(
        "a",
        &[],
        Some("body"),
        vec![quiz("a.q1", Difficulty::Easy)],
    )]);
    let p = path_with(&["a"]);
    let events = vec![taught("a"), answered("a.q1", Rating::Good, Utc::now())];
    assert_create_quiz(
        &scheduler::next_action(&g, &p, &PathProgress::from_events(&events), NO_DUE),
        "a",
        Difficulty::Medium,
    );
}

#[test]
fn advance_to_hard_after_easy_and_medium_correct() {
    let g = graph_of(vec![atom(
        "a",
        &[],
        Some("body"),
        vec![
            quiz("a.q1", Difficulty::Easy),
            quiz("a.q2", Difficulty::Medium),
        ],
    )]);
    let p = path_with(&["a"]);
    let events = vec![
        taught("a"),
        answered("a.q1", Rating::Easy, Utc::now()),
        answered("a.q2", Rating::Good, Utc::now()),
    ];
    assert_create_quiz(
        &scheduler::next_action(&g, &p, &PathProgress::from_events(&events), NO_DUE),
        "a",
        Difficulty::Hard,
    );
}

#[test]
fn done_after_all_three_correct_on_only_target() {
    let g = graph_of(vec![atom(
        "a",
        &[],
        Some("body"),
        vec![
            quiz("a.q1", Difficulty::Easy),
            quiz("a.q2", Difficulty::Medium),
            quiz("a.q3", Difficulty::Hard),
        ],
    )]);
    let p = path_with(&["a"]);
    let events = vec![
        taught("a"),
        answered("a.q1", Rating::Good, Utc::now()),
        answered("a.q2", Rating::Good, Utc::now()),
        answered("a.q3", Rating::Easy, Utc::now()),
    ];
    assert!(matches!(
        scheduler::next_action(&g, &p, &PathProgress::from_events(&events), NO_DUE),
        Action::Done
    ));
}

#[test]
fn advance_to_next_target_lesson_after_first_complete() {
    let g = graph_of(vec![
        atom(
            "a",
            &[],
            Some("body"),
            vec![
                quiz("a.q1", Difficulty::Easy),
                quiz("a.q2", Difficulty::Medium),
                quiz("a.q3", Difficulty::Hard),
            ],
        ),
        atom("b", &[], None, vec![]),
    ]);
    let p = path_with(&["a", "b"]);
    let events = vec![
        taught("a"),
        answered("a.q1", Rating::Good, Utc::now()),
        answered("a.q2", Rating::Good, Utc::now()),
        answered("a.q3", Rating::Good, Utc::now()),
    ];
    assert_create_lesson(
        &scheduler::next_action(&g, &p, &PathProgress::from_events(&events), NO_DUE),
        "b",
    );
}

#[test]
fn does_not_advance_after_only_lesson_stored() {
    // The exact regression: after `create_lesson` on `a`, `mt path next`
    // must NOT jump to `b`'s lesson. With the present_lesson step in
    // place, the precise next action is to re-surface `a`'s lesson
    // (since this path has no `LessonTaught` for it yet).
    let g = graph_of(vec![
        atom("a", &[], Some("body"), vec![]),
        atom("b", &[], None, vec![]),
    ]);
    let p = path_with(&["a", "b"]);
    assert_present_lesson(
        &scheduler::next_action(&g, &p, &PathProgress::default(), NO_DUE),
        "a",
    );
}

#[test]
fn descends_into_prereq_before_target() {
    let g = graph_of(vec![
        atom("pre", &[], None, vec![]),
        atom("target", &["pre"], Some("body"), vec![]),
    ]);
    let p = path_with(&["target"]);
    assert_create_lesson(
        &scheduler::next_action(&g, &p, &PathProgress::default(), NO_DUE),
        "pre",
    );
}

#[test]
fn finishes_prereq_quizzes_before_target_lesson() {
    let g = graph_of(vec![
        atom(
            "pre",
            &[],
            Some("body"),
            vec![quiz("pre.q1", Difficulty::Easy)],
        ),
        atom("target", &["pre"], None, vec![]),
    ]);
    let p = path_with(&["target"]);
    let events = vec![taught("pre")];
    assert_present_quiz(
        &scheduler::next_action(&g, &p, &PathProgress::from_events(&events), NO_DUE),
        "pre",
        "pre.q1",
    );
}

// ── is_atom_complete / atom_completed_at ───────────────────────────

#[test]
fn is_atom_complete_false_without_lesson() {
    let g = graph_of(vec![atom("a", &[], None, vec![])]);
    assert!(!scheduler::is_atom_complete(
        &g,
        &PathProgress::default(),
        "a"
    ));
}

#[test]
fn is_atom_complete_false_with_only_two_correct() {
    let g = graph_of(vec![atom(
        "a",
        &[],
        Some("body"),
        vec![
            quiz("a.q1", Difficulty::Easy),
            quiz("a.q2", Difficulty::Medium),
            quiz("a.q3", Difficulty::Hard),
        ],
    )]);
    let events = vec![
        answered("a.q1", Rating::Good, Utc::now()),
        answered("a.q2", Rating::Good, Utc::now()),
    ];
    assert!(!scheduler::is_atom_complete(
        &g,
        &PathProgress::from_events(&events),
        "a"
    ));
}

#[test]
fn is_atom_complete_true_with_all_three_correct() {
    let g = graph_of(vec![atom(
        "a",
        &[],
        Some("body"),
        vec![
            quiz("a.q1", Difficulty::Easy),
            quiz("a.q2", Difficulty::Medium),
            quiz("a.q3", Difficulty::Hard),
        ],
    )]);
    let events = vec![
        answered("a.q1", Rating::Good, Utc::now()),
        answered("a.q2", Rating::Easy, Utc::now()),
        answered("a.q3", Rating::Good, Utc::now()),
    ];
    assert!(scheduler::is_atom_complete(
        &g,
        &PathProgress::from_events(&events),
        "a"
    ));
}
