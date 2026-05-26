//! FSRS card state.
//!
//! Card state is stored twice:
//!
//! 1. An in-memory step, [apply_answer], takes current [CardState]
//!    plus a new rating/timestamp and produces the next state.
//!
//! 2. SQL helpers that read and write the cards table. This table is a
//!    cache of the latest FSRS state per `(path, quiz)` that allows for
//!    more efficient "next item" queries in the scheduler. The event log
//!    remains the source of truth and can rebuild the cache via `recompute`.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use fsrs::{FSRS, MemoryState};
use libsql::{Connection, params};
use serde::Serialize;

use crate::event_log::{Event, EventKind};
use crate::types::Rating;
use crate::{Error, Result};

const DESIRED_RETENTION: f32 = 0.9;

/// Derived FSRS state for a single quiz card.
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

/// Row stored in the `cards` table.
/// A card exists if a quiz has been answered at least once.
#[derive(Debug, Clone)]
pub struct CardRow {
    pub path_id: String,
    pub quiz_id: String,
    pub stability: f32,
    pub difficulty: f32,
    pub due_at: DateTime<Utc>,
    pub last_reviewed_at: DateTime<Utc>,
    pub reps: u32,
    pub lapses: u32,
}

impl CardRow {
    pub fn card_state(&self) -> CardState {
        CardState {
            due: self.due_at,
            last_review: Some(self.last_reviewed_at),
            stability: Some(self.stability),
            difficulty: Some(self.difficulty),
            last_rating: None,
        }
    }
}

// ── Pure FSRS step (no I/O) ────────────────────────────────────────

/// Replay one quiz's answered events to produce its current FSRS card
/// state. Returns `None` if the quiz has never been answered.
///
/// Kept primarily for the `recompute` rebuild path and for unit tests;
/// the runtime scheduler now reads pre-computed state from the `cards`
/// table via [`due_cards`].
pub fn card_state(events: &[Event], quiz_id: &str) -> Result<Option<CardState>> {
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
/// quiz. Used during cache rebuild (`recompute`) and in tests.
pub fn all_card_states(events: &[Event]) -> Result<HashMap<String, CardState>> {
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
) -> Result<CardState> {
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

    let fsrs = FSRS::new(Some(&[])).map_err(|e| Error::Fsrs(format!("{e:?}")))?;
    let next_states = fsrs
        .next_states(memory, DESIRED_RETENTION, days_elapsed)
        .map_err(|e| Error::Fsrs(format!("{e:?}")))?;
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

// ── SQL: read ──────────────────────────────────────────────────────

/// Load the cached row for one `(path, quiz)` pair, if present.
pub async fn read_card(conn: &Connection, path_id: &str, quiz_id: &str) -> Result<Option<CardRow>> {
    let mut rows = conn
        .query(
            "SELECT path_id, quiz_id, stability, difficulty, due_at, last_reviewed_at, reps, lapses \
             FROM cards WHERE path_id = ? AND quiz_id = ?",
            params![path_id.to_string(), quiz_id.to_string()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    Ok(Some(row_to_card(&row)?))
}

/// Return every `(quiz_id, due_at)` pair whose `due_at <= now` for the
/// given path, sorted oldest-due first. The scheduler uses this to pick
/// the earliest-due quiz without folding the event log.
pub async fn due_quizzes(
    conn: &Connection,
    path_id: &str,
    now: DateTime<Utc>,
) -> Result<Vec<(String, DateTime<Utc>)>> {
    let mut rows = conn
        .query(
            "SELECT quiz_id, due_at FROM cards \
             WHERE path_id = ? AND due_at <= ? ORDER BY due_at ASC",
            params![path_id.to_string(), now.to_rfc3339()],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        let quiz_id: String = row.get(0)?;
        let due_str: String = row.get(1)?;
        out.push((quiz_id, parse_ts(&due_str)?));
    }
    Ok(out)
}

// ── SQL: write-through cache update ────────────────────────────────

/// Apply one `QuizAnswered` event to the cards cache. Reads the previous
/// row (if any), folds the new rating through `apply_answer`, and upserts
/// the result with `reps` and `lapses` incremented accordingly.
///
/// Called from `event_log::append` when a `QuizAnswered` event lands.
///
/// # Panics
/// Panics if `apply_answer` returns a state without stability, difficulty,
/// or last-review — every code path inside `apply_answer` sets all three.
pub async fn apply_answer_to_cache(
    conn: &Connection,
    path_id: &str,
    quiz_id: &str,
    rating: Rating,
    ts: DateTime<Utc>,
) -> Result<()> {
    let prev = read_card(conn, path_id, quiz_id).await?;
    let prev_state = prev.as_ref().map(CardRow::card_state);
    let next = apply_answer(prev_state.as_ref(), rating, ts)?;

    let reps = prev.as_ref().map_or(0, |r| r.reps) + 1;
    let lapses = prev.as_ref().map_or(0, |r| r.lapses) + u32::from(rating == Rating::Again);

    upsert_card_row(
        conn,
        &CardRow {
            path_id: path_id.to_string(),
            quiz_id: quiz_id.to_string(),
            stability: next.stability.expect("apply_answer sets stability"),
            difficulty: next.difficulty.expect("apply_answer sets difficulty"),
            due_at: next.due,
            last_reviewed_at: next.last_review.expect("apply_answer sets last_review"),
            reps,
            lapses,
        },
    )
    .await
}

async fn upsert_card_row(conn: &Connection, row: &CardRow) -> Result<()> {
    conn.execute(
        "INSERT INTO cards(path_id, quiz_id, stability, difficulty, due_at, last_reviewed_at, reps, lapses) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(path_id, quiz_id) DO UPDATE SET \
            stability = excluded.stability, \
            difficulty = excluded.difficulty, \
            due_at = excluded.due_at, \
            last_reviewed_at = excluded.last_reviewed_at, \
            reps = excluded.reps, \
            lapses = excluded.lapses",
        params![
            row.path_id.as_str(),
            row.quiz_id.as_str(),
            f64::from(row.stability),
            f64::from(row.difficulty),
            row.due_at.to_rfc3339(),
            row.last_reviewed_at.to_rfc3339(),
            i64::from(row.reps),
            i64::from(row.lapses),
        ],
    )
    .await?;
    Ok(())
}

/// Drop every cached row for `path_id` and rebuild from the event log.
/// Use after a suspected cache corruption or a schema-altering migration.
pub async fn recompute(conn: &Connection, path_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM cards WHERE path_id = ?",
        params![path_id.to_string()],
    )
    .await?;

    // Stream `(quiz_id, rating, ts)` triples in chronological order.
    let mut rows = conn
        .query(
            "SELECT quiz_id, rating, ts FROM events \
             WHERE path_id = ? AND kind = 'quiz_answered' AND quiz_id IS NOT NULL AND rating IS NOT NULL \
             ORDER BY id ASC",
            params![path_id.to_string()],
        )
        .await?;

    while let Some(row) = rows.next().await? {
        let quiz_id: String = row.get(0)?;
        let rating_int: i64 = row.get(1)?;
        let ts_str: String = row.get(2)?;
        let rating = Rating::try_from(rating_int)?;
        let ts = parse_ts(&ts_str)?;
        apply_answer_to_cache(conn, path_id, &quiz_id, rating, ts).await?;
    }
    Ok(())
}

// ── helpers ────────────────────────────────────────────────────────

#[allow(clippy::cast_possible_truncation)]
fn row_to_card(row: &libsql::Row) -> Result<CardRow> {
    let path_id: String = row.get(0)?;
    let quiz_id: String = row.get(1)?;
    let stability_f: f64 = row.get(2)?;
    let difficulty_f: f64 = row.get(3)?;
    let due_str: String = row.get(4)?;
    let last_reviewed_str: String = row.get(5)?;
    let reps: i64 = row.get(6)?;
    let lapses: i64 = row.get(7)?;
    Ok(CardRow {
        path_id,
        quiz_id,
        stability: stability_f as f32,
        difficulty: difficulty_f as f32,
        due_at: parse_ts(&due_str)?,
        last_reviewed_at: parse_ts(&last_reviewed_str)?,
        reps: u32::try_from(reps)
            .map_err(|_| Error::CardsCorrupt(format!("invalid reps {reps}")))?,
        lapses: u32::try_from(lapses)
            .map_err(|_| Error::CardsCorrupt(format!("invalid lapses {lapses}")))?,
    })
}

fn parse_ts(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| Error::BadTimestamp(format!("{s}: {e}")))
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
