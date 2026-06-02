//! Tests for `tree::state_badge` — the per-atom `[LEMH]` glyph rendered
//! by `mt path tree` and the MCP tree view.

use chrono::Utc;

use mathtutor::event_log::{Event, EventKind, EventPayload};
use mathtutor::graph::{FlatConcept, Quiz};
use mathtutor::progress::PathProgress;
use mathtutor::tree::state_badge;
use mathtutor::types::{Difficulty, Rating};

mod common;

const PATH_ID: &str = "p_test";

fn atom(id: &str, lesson: Option<&str>, quizzes: Vec<Quiz>) -> FlatConcept {
    FlatConcept {
        id: id.into(),
        name: id.into(),
        description: None,
        prerequisites: Vec::new(),
        children_ids: Vec::new(),
        lesson: lesson.map(String::from),
        quizzes,
    }
}

fn quiz(id: &str, difficulty: Difficulty) -> Quiz {
    Quiz {
        id: id.into(),
        difficulty,
        kind: None,
        question: "q".into(),
        answer: "a".into(),
        rubric: None,
    }
}

fn answered(quiz_id: &str, rating: Rating) -> Event {
    Event {
        ts: Utc::now(),
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

#[test]
fn empty_when_nothing_stored() {
    let a = atom("a", None, vec![]);
    assert_eq!(state_badge(&PathProgress::default(), &a), "[····]");
}

#[test]
fn lesson_slot_only_lights_up_after_taught() {
    let a = atom("a", Some("body"), vec![]);
    // Body exists in the graph but no `LessonTaught` event for this path
    // yet — `L` stays unlit.
    assert_eq!(state_badge(&PathProgress::default(), &a), "[····]");
    let events = vec![taught("a")];
    assert_eq!(state_badge(&common::progress_of(&events), &a), "[L···]");
}

#[test]
fn lowercase_when_quiz_authored_but_unanswered() {
    let a = atom(
        "a",
        Some("body"),
        vec![
            quiz("a.q1", Difficulty::Easy),
            quiz("a.q2", Difficulty::Medium),
        ],
    );
    let events = vec![taught("a")];
    assert_eq!(state_badge(&common::progress_of(&events), &a), "[Lem·]");
}

#[test]
fn uppercase_when_quiz_answered_correctly() {
    let a = atom(
        "a",
        Some("body"),
        vec![
            quiz("a.q1", Difficulty::Easy),
            quiz("a.q2", Difficulty::Medium),
            quiz("a.q3", Difficulty::Hard),
        ],
    );
    let events = vec![
        taught("a"),
        answered("a.q1", Rating::Good),
        answered("a.q2", Rating::Easy),
        answered("a.q3", Rating::Again), // wrong → stays lowercase
    ];
    assert_eq!(state_badge(&common::progress_of(&events), &a), "[LEMh]");
}
