//! Unit tests for `cards::apply_answer`.

use chrono::{Duration, TimeZone, Utc};
use mathtutor::cards::apply_answer;
use mathtutor::types::Rating;

#[test]
fn apply_answer_updates_last_review_each_step() {
    let t1 = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
    let t2 = t1 + Duration::days(30);

    let s1 = apply_answer(None, Rating::Good, t1).unwrap();
    assert_eq!(s1.last_review, t1);

    let s2 = apply_answer(Some(&s1), Rating::Good, t2).unwrap();
    assert_eq!(
        s2.last_review, t2,
        "last_review must advance to the current answer's ts, not stay on the first"
    );
}

#[test]
fn chained_steps_use_gap_to_most_recent_answer_not_first() {
    let t1 = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
    let t2 = t1 + Duration::days(30);
    let t3 = t2 + Duration::days(60);

    let three_step = {
        let s1 = apply_answer(None, Rating::Good, t1).unwrap();
        let s2 = apply_answer(Some(&s1), Rating::Good, t2).unwrap();
        apply_answer(Some(&s2), Rating::Good, t3).unwrap()
    };
    let two_step_skipping_middle = {
        let s1 = apply_answer(None, Rating::Good, t1).unwrap();
        apply_answer(Some(&s1), Rating::Good, t3).unwrap()
    };

    assert!(
        (three_step.stability - two_step_skipping_middle.stability).abs() > 1e-6,
        "three answers (gaps 30, 60) must differ from two answers (gap 90); \
         otherwise the third step is using gap-to-first instead of gap-to-prev"
    );
}

#[test]
fn replay_rejects_a_quiz_answer_without_a_rating() {
    let event = mathtutor::event_log::Event {
        ts: chrono::Utc::now(),
        kind: mathtutor::event_log::EventKind::QuizAnswered,
        path: "p".into(),
        atom: None,
        quiz: Some("p.q1".into()),
        payload: mathtutor::event_log::EventPayload::default(),
    };
    let e = mathtutor::cards::fold_history("p", &[event]).unwrap_err();
    assert!(matches!(e, mathtutor::Error::BadEvent(_)), "{e}");
}

#[test]
fn replay_rejects_a_quiz_event_without_a_quiz_like_a_live_append() {
    let event = mathtutor::event_log::Event {
        ts: chrono::Utc::now(),
        kind: mathtutor::event_log::EventKind::QuizAnswered,
        path: "p".into(),
        atom: None,
        quiz: None,
        payload: mathtutor::event_log::EventPayload {
            rating: Some(mathtutor::types::Rating::Good),
            ..Default::default()
        },
    };
    let e = mathtutor::cards::fold_history("p", &[event]).unwrap_err();
    assert!(
        matches!(e, mathtutor::Error::BadEvent("quiz event without quiz")),
        "{e}"
    );
}
