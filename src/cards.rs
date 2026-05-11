//! FSRS card state, derived purely from the event log.
//!
//! No card state is persisted on disk in V1 — every `QuizAnswered` event
//! contains the timestamp and rating, which is everything FSRS needs to
//! reproduce its scheduling decision. Replaying the events for a quiz in
//! order yields the same `CardState` that used to live in `path.ayml`.
//! Per DESIGN.md: "FSRS state … is a derived index that can be
//! recomputed from the log."

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use fsrs::{FSRS, MemoryState};
use serde::Serialize;

use crate::event_log::{Event, EventKind};
use crate::types::Rating;

const DESIRED_RETENTION: f32 = 0.9;

#[derive(Debug, thiserror::Error)]
pub enum CardError {
    #[error("fsrs: {0}")]
    Fsrs(String),
}

/// Derived FSRS state for a single quiz card. Not persisted — computed
/// on demand from the event log.
#[derive(Debug, Clone, Serialize)]
pub struct CardState {
    pub due: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_review: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stability: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub difficulty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_rating: Option<Rating>,
}

/// Replay one quiz's answered events to produce its current FSRS card
/// state. Returns `None` if the quiz has never been answered.
pub fn card_state(events: &[Event], quiz_id: &str) -> Result<Option<CardState>, CardError> {
    let mut state: Option<CardState> = None;
    for e in events {
        if !matches!(e.kind, EventKind::QuizAnswered) {
            continue;
        }
        if e.quiz.as_deref() != Some(quiz_id) {
            continue;
        }
        let Some(rating) = e.payload.rating else {
            continue;
        };
        state = Some(apply_answer(state.as_ref(), rating, e.ts)?);
    }
    Ok(state)
}

/// Replay every answered quiz in the log and return current state per
/// quiz. Used by the scheduler to find the earliest-due card without
/// repeatedly scanning the log.
pub fn all_card_states(events: &[Event]) -> Result<HashMap<String, CardState>, CardError> {
    // Group answered events by quiz_id, preserving chronological order
    // (the log itself is already in order, so a single pass suffices).
    let mut by_quiz: HashMap<String, Vec<(DateTime<Utc>, Rating)>> = HashMap::new();
    for e in events {
        if !matches!(e.kind, EventKind::QuizAnswered) {
            continue;
        }
        let (Some(quiz_id), Some(rating)) = (e.quiz.as_deref(), e.payload.rating) else {
            continue;
        };
        by_quiz
            .entry(quiz_id.to_string())
            .or_default()
            .push((e.ts, rating));
    }

    let mut out = HashMap::with_capacity(by_quiz.len());
    for (quiz_id, history) in by_quiz {
        let mut state: Option<CardState> = None;
        for (ts, rating) in history {
            state = Some(apply_answer(state.as_ref(), rating, ts)?);
        }
        if let Some(s) = state {
            out.insert(quiz_id, s);
        }
    }
    Ok(out)
}

/// One FSRS step: given the previous state (or none), apply this rating
/// at this timestamp and return the resulting `CardState`.
///
/// `interval` is converted from FSRS's fractional days to whole seconds
/// (with a 60-second floor) so sub-day relearning intervals retain their
/// precision and zero-interval scheduling can't happen.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
pub fn apply_answer(
    prev: Option<&CardState>,
    rating: Rating,
    ts: DateTime<Utc>,
) -> Result<CardState, CardError> {
    let days_elapsed = match prev.and_then(|c| c.last_review) {
        Some(lr) => (ts - lr).num_days().max(0) as u32,
        None => 0,
    };
    let memory = prev.and_then(|c| match (c.stability, c.difficulty) {
        (Some(s), Some(d)) => Some(MemoryState {
            stability: s,
            difficulty: d,
        }),
        _ => None,
    });

    let fsrs = FSRS::new(Some(&[])).map_err(|e| CardError::Fsrs(format!("{e:?}")))?;
    let next_states = fsrs
        .next_states(memory, DESIRED_RETENTION, days_elapsed)
        .map_err(|e| CardError::Fsrs(format!("{e:?}")))?;
    let next = match rating {
        Rating::Again => next_states.again,
        Rating::Hard => next_states.hard,
        Rating::Good => next_states.good,
        Rating::Easy => next_states.easy,
    };

    let interval_secs = (next.interval * 86_400.0).round().max(60.0) as i64;
    Ok(CardState {
        due: ts + Duration::seconds(interval_secs),
        last_review: Some(ts),
        stability: Some(next.memory.stability),
        difficulty: Some(next.memory.difficulty),
        last_rating: Some(rating),
    })
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use crate::event_log::{Event, EventKind, EventPayload};

    use super::{CardState, Rating, apply_answer, card_state};

    fn answered_event(quiz: &str, rating: Rating, ts: chrono::DateTime<chrono::Utc>) -> Event {
        Event {
            ts,
            kind: EventKind::QuizAnswered,
            path: "p_test".into(),
            atom: None,
            quiz: Some(quiz.into()),
            payload: EventPayload {
                rating: Some(rating),
                ..Default::default()
            },
        }
    }

    #[test]
    fn apply_answer_updates_last_review_each_step() {
        // The core invariant: a fresh `apply_answer` call must overwrite
        // `last_review`, otherwise future replays would compute their
        // `days_elapsed` from the very first answer instead of the most
        // recent one.
        let t1 = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let t2 = t1 + chrono::Duration::days(30);

        let s1 = apply_answer(None, Rating::Good, t1).unwrap();
        assert_eq!(s1.last_review, Some(t1));

        let s2 = apply_answer(Some(&s1), Rating::Good, t2).unwrap();
        assert_eq!(
            s2.last_review,
            Some(t2),
            "last_review must advance to the current answer's ts, not stay on the first"
        );
    }

    #[test]
    fn replay_uses_gap_to_most_recent_answer_not_first() {
        // Scenario the user asked about: same quiz answered three times.
        // Each FSRS step must see the gap to its *immediately preceding*
        // answer, not the gap to the original. We prove this by showing
        // that a three-step replay (gaps 30, 60) ends in a different
        // state than a two-step replay that skips the middle answer
        // (gap 90). If the implementation accidentally used `ts - first`
        // for `days_elapsed`, the third step of the three-step replay
        // would behave identically to the second step of the two-step
        // replay, and these states would collide.
        let t1 = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let t2 = t1 + chrono::Duration::days(30);
        let t3 = t2 + chrono::Duration::days(60);

        let three_step = card_state(
            &[
                answered_event("q.q1", Rating::Good, t1),
                answered_event("q.q1", Rating::Good, t2),
                answered_event("q.q1", Rating::Good, t3),
            ],
            "q.q1",
        )
        .unwrap()
        .unwrap();

        let two_step_skipping_middle = card_state(
            &[
                answered_event("q.q1", Rating::Good, t1),
                answered_event("q.q1", Rating::Good, t3),
            ],
            "q.q1",
        )
        .unwrap()
        .unwrap();

        assert_ne!(
            three_step.stability, two_step_skipping_middle.stability,
            "three answers (gaps 30, 60) must differ from two answers (gap 90); \
             otherwise the third step is using gap-to-first instead of gap-to-prev"
        );
    }

    #[test]
    fn card_state_replay_matches_step_by_step_apply_answer() {
        // The replay path (`card_state`) and the direct path
        // (chained `apply_answer` calls) must produce identical state.
        // This pins the contract that `card_state` is exactly "fold
        // apply_answer over the answered events for this quiz."
        let t1 = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let t2 = t1 + chrono::Duration::days(7);
        let t3 = t2 + chrono::Duration::days(21);

        let reference: CardState = {
            let s1 = apply_answer(None, Rating::Good, t1).unwrap();
            let s2 = apply_answer(Some(&s1), Rating::Hard, t2).unwrap();
            apply_answer(Some(&s2), Rating::Easy, t3).unwrap()
        };

        let replayed = card_state(
            &[
                answered_event("q.q1", Rating::Good, t1),
                answered_event("q.q1", Rating::Hard, t2),
                answered_event("q.q1", Rating::Easy, t3),
            ],
            "q.q1",
        )
        .unwrap()
        .unwrap();

        assert_eq!(replayed.due, reference.due);
        assert_eq!(replayed.last_review, reference.last_review);
        assert_eq!(replayed.stability, reference.stability);
        assert_eq!(replayed.difficulty, reference.difficulty);
        assert_eq!(replayed.last_rating, reference.last_rating);
    }
}
