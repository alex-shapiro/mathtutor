//! Unit tests for `PathPlan::next_action` and `scheduler::is_atom_complete`.

use chrono::{DateTime, Duration, Utc};

use mathtutor::graph::Graph;
use mathtutor::progress::{PathProgress, QuizSlot};
use mathtutor::scheduler::{self, Action};
use mathtutor::types::{Difficulty, Rating};

mod common;

use common::{atom, cluster, path_with, quiz, taught};

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

#[test]
fn create_lesson_if_atom_lacks_lesson() {
    let g = Graph::of_concepts(vec![atom("a", &[], None, vec![])]);
    let p = path_with(&["a"]);
    assert_create_lesson(
        &common::next_action(&g, &p, &[], scheduler::NextMode::Default),
        "a",
    );
}

#[test]
fn present_lesson_if_stored_but_untaught_in_path() {
    let g = Graph::of_concepts(vec![atom("a", &[], Some("body"), vec![])]);
    let p = path_with(&["a"]);
    assert_present_lesson(
        &common::next_action(&g, &p, &[], scheduler::NextMode::Default),
        "a",
    );
}

#[test]
fn lesson_authored_event_satisfies_taught_check() {
    let g = Graph::of_concepts(vec![atom("a", &[], Some("body"), vec![])]);
    let p = path_with(&["a"]);
    let events = vec![common::lesson_authored("a")];
    assert_create_quiz(
        &common::next_action(&g, &p, &events, scheduler::NextMode::Default),
        "a",
        Difficulty::Easy,
    );
}

#[test]
fn create_easy_quiz_if_lesson_stored_and_taught() {
    let g = Graph::of_concepts(vec![atom("a", &[], Some("body"), vec![])]);
    let p = path_with(&["a"]);
    let events = vec![taught("a")];
    assert_create_quiz(
        &common::next_action(&g, &p, &events, scheduler::NextMode::Default),
        "a",
        Difficulty::Easy,
    );
}

#[test]
fn present_easy_quiz_if_authored_but_unanswered() {
    let g = Graph::of_concepts(vec![atom(
        "a",
        &[],
        Some("body"),
        vec![quiz("a.q1", Difficulty::Easy)],
    )]);
    let p = path_with(&["a"]);
    let events = vec![taught("a")];
    assert_present_quiz(
        &common::next_action(&g, &p, &events, scheduler::NextMode::Default),
        "a",
        "a.q1",
    );
}

#[test]
fn keep_presenting_easy_after_again_rating() {
    let g = Graph::of_concepts(vec![atom(
        "a",
        &[],
        Some("body"),
        vec![quiz("a.q1", Difficulty::Easy)],
    )]);
    let p = path_with(&["a"]);
    let events = vec![taught("a"), common::answered("a.q1", Rating::Again)];
    assert_present_quiz(
        &common::next_action(&g, &p, &events, scheduler::NextMode::Default),
        "a",
        "a.q1",
    );
}

#[test]
fn advance_to_medium_after_hard_rating() {
    let g = Graph::of_concepts(vec![atom(
        "a",
        &[],
        Some("body"),
        vec![quiz("a.q1", Difficulty::Easy)],
    )]);
    let p = path_with(&["a"]);
    let events = vec![taught("a"), common::answered("a.q1", Rating::Hard)];
    assert_create_quiz(
        &common::next_action(&g, &p, &events, scheduler::NextMode::Default),
        "a",
        Difficulty::Medium,
    );
}

#[test]
fn advance_to_medium_after_easy_correct() {
    let g = Graph::of_concepts(vec![atom(
        "a",
        &[],
        Some("body"),
        vec![quiz("a.q1", Difficulty::Easy)],
    )]);
    let p = path_with(&["a"]);
    let events = vec![taught("a"), common::answered("a.q1", Rating::Good)];
    assert_create_quiz(
        &common::next_action(&g, &p, &events, scheduler::NextMode::Default),
        "a",
        Difficulty::Medium,
    );
}

#[test]
fn advance_to_hard_after_easy_and_medium_correct() {
    let g = Graph::of_concepts(vec![atom(
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
        common::answered("a.q1", Rating::Easy),
        common::answered("a.q2", Rating::Good),
    ];
    assert_create_quiz(
        &common::next_action(&g, &p, &events, scheduler::NextMode::Default),
        "a",
        Difficulty::Hard,
    );
}

#[test]
fn done_after_all_three_correct_on_only_target() {
    let g = Graph::of_concepts(vec![atom(
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
        common::answered("a.q1", Rating::Good),
        common::answered("a.q2", Rating::Good),
        common::answered("a.q3", Rating::Easy),
    ];
    assert!(matches!(
        common::next_action(&g, &p, &events, scheduler::NextMode::Default),
        Action::Done
    ));
}

#[test]
fn advance_to_next_target_lesson_after_first_complete() {
    let g = Graph::of_concepts(vec![
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
        common::answered("a.q1", Rating::Good),
        common::answered("a.q2", Rating::Good),
        common::answered("a.q3", Rating::Good),
    ];
    assert_create_lesson(
        &common::next_action(&g, &p, &events, scheduler::NextMode::Default),
        "b",
    );
}

#[test]
fn does_not_advance_after_only_lesson_stored() {
    let g = Graph::of_concepts(vec![
        atom("a", &[], Some("body"), vec![]),
        atom("b", &[], None, vec![]),
    ]);
    let p = path_with(&["a", "b"]);
    assert_present_lesson(
        &common::next_action(&g, &p, &[], scheduler::NextMode::Default),
        "a",
    );
}

#[test]
fn descends_into_prereq_before_target() {
    let g = Graph::of_concepts(vec![
        atom("pre", &[], None, vec![]),
        atom("target", &["pre"], Some("body"), vec![]),
    ]);
    let p = path_with(&["target"]);
    assert_create_lesson(
        &common::next_action(&g, &p, &[], scheduler::NextMode::Default),
        "pre",
    );
}

#[test]
fn finishes_prereq_quizzes_before_target_lesson() {
    let g = Graph::of_concepts(vec![
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
        &common::next_action(&g, &p, &events, scheduler::NextMode::Default),
        "pre",
        "pre.q1",
    );
}

#[test]
fn cluster_target_descends_to_first_atom() {
    let g = Graph::of_concepts(vec![
        cluster("c", &["c.1", "c.2"]),
        atom("c.1", &[], None, vec![]),
        atom("c.2", &[], None, vec![]),
    ]);
    let p = path_with(&["c"]);
    assert_create_lesson(
        &common::next_action(&g, &p, &[], scheduler::NextMode::Default),
        "c.1",
    );
}

#[test]
fn cluster_prereq_descends_before_target() {
    let g = Graph::of_concepts(vec![
        cluster("pre", &["pre.1", "pre.2"]),
        atom("pre.1", &[], None, vec![]),
        atom("pre.2", &[], None, vec![]),
        atom("target", &["pre"], None, vec![]),
    ]);
    let p = path_with(&["target"]);
    assert_create_lesson(
        &common::next_action(&g, &p, &[], scheduler::NextMode::Default),
        "pre.1",
    );
}

#[test]
fn cluster_prereq_advances_to_target_once_complete() {
    let g = Graph::of_concepts(vec![
        cluster("pre", &["pre.1"]),
        common::complete_atom("pre.1", &[]),
        atom("target", &["pre"], None, vec![]),
    ]);
    let p = path_with(&["target"]);
    let events = common::complete_events("pre.1");
    assert_create_lesson(
        &common::next_action(&g, &p, &events, scheduler::NextMode::Default),
        "target",
    );
}

#[test]
fn is_atom_complete_false_without_lesson() {
    let g = Graph::of_concepts(vec![atom("a", &[], None, vec![])]);
    assert!(!scheduler::is_atom_complete(
        &g,
        &PathProgress::default(),
        "a"
    ));
}

#[test]
fn is_atom_complete_false_with_only_two_correct() {
    let g = Graph::of_concepts(vec![atom(
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
        common::answered("a.q1", Rating::Good),
        common::answered("a.q2", Rating::Good),
    ];
    assert!(!scheduler::is_atom_complete(
        &g,
        &common::progress_of(&events),
        "a"
    ));
}

#[test]
fn is_atom_complete_true_with_all_three_correct() {
    let g = Graph::of_concepts(vec![atom(
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
        taught("a"),
        common::answered("a.q1", Rating::Good),
        common::answered("a.q2", Rating::Easy),
        common::answered("a.q3", Rating::Good),
    ];
    assert!(scheduler::is_atom_complete(
        &g,
        &common::progress_of(&events),
        "a"
    ));
}

#[test]
fn passed_quizzes_do_not_complete_an_atom_whose_lesson_is_untaught() {
    let g = Graph::of_concepts(vec![common::complete_atom("a", &[])]);
    let p = path_with(&["a"]);
    let events = vec![
        common::answered("a.q1", Rating::Good),
        common::answered("a.q2", Rating::Good),
        common::answered("a.q3", Rating::Good),
    ];
    let progress = common::progress_of(&events);

    assert!(!scheduler::is_atom_complete(&g, &progress, "a"));
    assert_present_lesson(
        &common::next_action(&g, &p, &events, scheduler::NextMode::Default),
        "a",
    );
}

#[test]
fn a_passed_second_quiz_of_a_difficulty_completes_the_slot() {
    let mut a = common::complete_atom("a", &[]);
    a.quizzes.insert(1, quiz("a.q4", Difficulty::Easy));
    let g = Graph::of_concepts(vec![a]);
    let p = path_with(&["a"]);
    let events = vec![taught("a"), common::answered("a.q4", Rating::Good)];
    let progress = common::progress_of(&events);

    assert_eq!(
        progress.quiz_slot(&g.by_id["a"], Difficulty::Easy),
        QuizSlot::Correct
    );
    assert_present_quiz(
        &common::next_action(&g, &p, &events, scheduler::NextMode::Default),
        "a",
        "a.q2",
    );

    let mut events = events;
    events.push(common::answered("a.q2", Rating::Good));
    events.push(common::answered("a.q3", Rating::Good));
    assert!(scheduler::is_atom_complete(
        &g,
        &common::progress_of(&events),
        "a"
    ));
}

/// Return a time at which every card that the tests answer is due.
fn later() -> DateTime<Utc> {
    Utc::now() + Duration::days(3650)
}

#[test]
fn new_mode_skips_due_card_and_returns_walker_action() {
    let g = Graph::of_concepts(vec![
        atom("a", &[], Some("body"), vec![quiz("a.q1", Difficulty::Easy)]),
        atom("b", &[], None, vec![]),
    ]);
    let p = path_with(&["a", "b"]);
    let events = vec![taught("a"), common::answered("a.q1", Rating::Good)];
    assert_create_quiz(
        &common::next_action_at(&g, &p, &events, later(), scheduler::NextMode::New),
        "a",
        Difficulty::Medium,
    );
}

#[test]
fn due_mode_returns_earliest_due_card() {
    let g = Graph::of_concepts(vec![atom(
        "a",
        &[],
        Some("body"),
        vec![quiz("a.q1", Difficulty::Easy)],
    )]);
    let p = path_with(&["a"]);
    let events = vec![taught("a"), common::answered("a.q1", Rating::Good)];
    assert_present_quiz(
        &common::next_action_at(&g, &p, &events, later(), scheduler::NextMode::Due),
        "a",
        "a.q1",
    );
}

#[test]
fn due_mode_reports_zero_due_reviews_on_an_incomplete_path() {
    let g = Graph::of_concepts(vec![atom("a", &[], None, vec![])]);
    let p = path_with(&["a"]);
    assert!(matches!(
        common::next_action(&g, &p, &[], scheduler::NextMode::Due),
        Action::NoReviewsDue
    ));
}

#[test]
fn from_flags_rejects_both_new_and_due() {
    assert!(matches!(
        scheduler::NextMode::from_flags(true, true),
        Err(mathtutor::Error::ConflictingFlags("--new", "--due"))
    ));
    assert_eq!(
        scheduler::NextMode::from_flags(true, false).unwrap(),
        scheduler::NextMode::New,
    );
    assert_eq!(
        scheduler::NextMode::from_flags(false, true).unwrap(),
        scheduler::NextMode::Due,
    );
    assert_eq!(
        scheduler::NextMode::from_flags(false, false).unwrap(),
        scheduler::NextMode::Default,
    );
}

#[test]
fn new_mode_returns_done_if_all_targets_are_complete() {
    let g = Graph::of_concepts(vec![atom(
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
        common::answered("a.q1", Rating::Good),
        common::answered("a.q2", Rating::Good),
        common::answered("a.q3", Rating::Good),
    ];
    assert!(matches!(
        common::next_action_at(&g, &p, &events, later(), scheduler::NextMode::New),
        Action::Done
    ));
}

#[test]
fn due_card_of_a_removed_atom_does_not_hide_later_due_cards() {
    let g = Graph::of_concepts(vec![atom(
        "a",
        &[],
        Some("body"),
        vec![quiz("a.q1", Difficulty::Easy)],
    )]);
    let p = path_with(&["a"]);
    let events = vec![
        common::at(
            Utc::now() - Duration::days(1),
            common::answered("removed.q1", Rating::Good),
        ),
        common::answered("a.q1", Rating::Good),
    ];

    let action = common::next_action_at(&g, &p, &events, later(), scheduler::NextMode::Due);

    assert_present_quiz(&action, "a", "a.q1");
}
