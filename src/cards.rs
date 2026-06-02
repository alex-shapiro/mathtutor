//! FSRS card state.
//!
//! Card state lives in two places:
//!
//! 1. An in-memory step, [`apply_answer`], takes a previous [`CardState`]
//!    plus a new rating/timestamp and produces the next state. Pure FSRS
//!    — no I/O.
//!
//! 2. SQL helpers read and write the `cards` table, a write-through cache
//!    of the latest FSRS state per `(path, quiz)` so the scheduler can
//!    pick the earliest-due quiz with one indexed query. The event log
//!    remains the source of truth; [`recompute`] rebuilds the cache by
//!    replaying every `QuizAnswered` event for a path.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use fsrs::{FSRS, MemoryState};
use libsql::{Connection, params};

use crate::db;
use crate::event_log::{self, Event, EventKind};
use crate::types::Rating;
use crate::{Error, Result};

const DESIRED_RETENTION: f32 = 0.9;

/// FSRS state for a single quiz card. The fields exist precisely when a
/// card has been answered at least once — `apply_answer(None, …)` is the
/// first step that produces a state, so every field is always populated.
#[derive(Debug, Clone, Copy)]
pub struct CardState {
    pub due: DateTime<Utc>,
    pub last_review: DateTime<Utc>,
    pub stability: f32,
    pub difficulty: f32,
}

/// Row shape stored in the `cards` table.
#[derive(Debug, Clone)]
pub struct CardRow {
    pub path_id: String,
    pub quiz_id: String,
    pub state: CardState,
    pub reps: u32,
    pub lapses: u32,
}

// ── Pure FSRS step (no I/O) ────────────────────────────────────────

/// One FSRS step: given the previous state (or none, for the first
/// answer), apply this rating at this timestamp and return the next
/// state.
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
    let days_elapsed = match prev {
        Some(c) => (ts - c.last_review).num_days().max(0) as u32,
        None => 0,
    };
    let memory = prev.map(|c| MemoryState {
        stability: c.stability,
        difficulty: c.difficulty,
    });

    let fsrs = FSRS::new(&[]).map_err(|e| Error::Fsrs(format!("{e:?}")))?;
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
        last_review: ts,
        stability: next.memory.stability,
        difficulty: next.memory.difficulty,
    })
}

// ── SQL: read ──────────────────────────────────────────────────────

/// Read `(reps, lapses)` for one `(path, quiz)` pair without parsing
/// the FSRS state columns. Callers that only need answer counts should
/// prefer this over [`read_card`].
pub async fn read_counts(
    conn: &Connection,
    path_id: &str,
    quiz_id: &str,
) -> Result<Option<(u32, u32)>> {
    let mut rows = conn
        .query(
            "SELECT reps, lapses FROM cards WHERE path_id = ? AND quiz_id = ?",
            params![path_id, quiz_id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let reps: i64 = row.get(0)?;
    let lapses: i64 = row.get(1)?;
    Ok(Some((
        u32::try_from(reps).map_err(|_| Error::CardsCorrupt(format!("invalid reps {reps}")))?,
        u32::try_from(lapses)
            .map_err(|_| Error::CardsCorrupt(format!("invalid lapses {lapses}")))?,
    )))
}

/// Load the cached row for one `(path, quiz)` pair, if present.
pub async fn read_card(conn: &Connection, path_id: &str, quiz_id: &str) -> Result<Option<CardRow>> {
    let mut rows = conn
        .query(
            "SELECT path_id, quiz_id, stability, difficulty, due_at, last_reviewed_at, reps, lapses \
             FROM cards WHERE path_id = ? AND quiz_id = ?",
            params![path_id, quiz_id],
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
            params![path_id, db::format_ts(now)],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        let quiz_id: String = row.get(0)?;
        let due_str: String = row.get(1)?;
        out.push((quiz_id, db::parse_ts(&due_str)?));
    }
    Ok(out)
}

// ── SQL: write-through cache update ────────────────────────────────

/// Apply one `QuizAnswered` event to the cards cache. Reads the previous
/// row (if any), folds the new rating through [`apply_answer`], and upserts
/// the result with `reps` and `lapses` incremented accordingly.
///
/// Called from [`event_log::append`] when a `QuizAnswered` event lands.
pub async fn apply_answer_to_cache(
    conn: &Connection,
    path_id: &str,
    quiz_id: &str,
    rating: Rating,
    ts: DateTime<Utc>,
) -> Result<()> {
    let prev = read_card(conn, path_id, quiz_id).await?;
    let next = apply_answer(prev.as_ref().map(|r| &r.state), rating, ts)?;

    let reps = prev.as_ref().map_or(0, |r| r.reps) + 1;
    let lapses = prev.as_ref().map_or(0, |r| r.lapses) + u32::from(rating == Rating::Again);

    upsert_card_row(conn, path_id, quiz_id, &next, reps, lapses).await
}

async fn upsert_card_row(
    conn: &Connection,
    path_id: &str,
    quiz_id: &str,
    state: &CardState,
    reps: u32,
    lapses: u32,
) -> Result<()> {
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
            path_id,
            quiz_id,
            f64::from(state.stability),
            f64::from(state.difficulty),
            db::format_ts(state.due),
            db::format_ts(state.last_review),
            i64::from(reps),
            i64::from(lapses),
        ],
    )
    .await?;
    Ok(())
}

/// Drop every cached row for `path_id` and rebuild from the event log.
/// Use after a suspected cache corruption or a schema-altering migration.
///
/// The replay is wrapped in one transaction — including the event-log
/// read — so concurrent `QuizAnswered` appends can't slip in between
/// the snapshot we fold and the rows we write back, and a mid-rebuild
/// failure leaves the existing cache in place rather than half-erased.
pub async fn recompute(conn: &Connection, path_id: &str) -> Result<()> {
    let tx = conn.transaction().await?;
    let events = event_log::load(&tx, path_id).await?;
    let rebuilt = fold_history(path_id, &events)?;
    tx.execute("DELETE FROM cards WHERE path_id = ?", params![path_id])
        .await?;
    for row in &rebuilt {
        upsert_card_row(
            &tx,
            &row.path_id,
            &row.quiz_id,
            &row.state,
            row.reps,
            row.lapses,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Fold every `QuizAnswered` event for `path_id` into one [`CardRow`] per
/// quiz. The inverse of `apply_answer_to_cache`: same FSRS step, applied
/// in chronological order, but in memory and one quiz at a time so a
/// full rebuild does K upserts instead of N.
fn fold_history(path_id: &str, events: &[Event]) -> Result<Vec<CardRow>> {
    // Group answered events per quiz, in event-log order. The log is
    // already chronological — `ORDER BY id ASC` in `event_log::load` —
    // so a single pass suffices to bucket the history.
    let mut history: HashMap<String, Vec<(DateTime<Utc>, Rating)>> = HashMap::new();
    for e in events {
        if e.kind != EventKind::QuizAnswered {
            continue;
        }
        let (Some(quiz), Some(rating)) = (e.quiz.as_deref(), e.payload.rating) else {
            continue;
        };
        history
            .entry(quiz.to_string())
            .or_default()
            .push((e.ts, rating));
    }

    let mut out = Vec::with_capacity(history.len());
    for (quiz_id, ratings) in history {
        let mut state: Option<CardState> = None;
        let mut reps: u32 = 0;
        let mut lapses: u32 = 0;
        for (ts, rating) in ratings {
            state = Some(apply_answer(state.as_ref(), rating, ts)?);
            reps += 1;
            if rating == Rating::Again {
                lapses += 1;
            }
        }
        out.push(CardRow {
            path_id: path_id.to_string(),
            quiz_id,
            state: state.expect("at least one answered event in the group"),
            reps,
            lapses,
        });
    }
    Ok(out)
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
        state: CardState {
            stability: stability_f as f32,
            difficulty: difficulty_f as f32,
            due: db::parse_ts(&due_str)?,
            last_review: db::parse_ts(&last_reviewed_str)?,
        },
        reps: u32::try_from(reps)
            .map_err(|_| Error::CardsCorrupt(format!("invalid reps {reps}")))?,
        lapses: u32::try_from(lapses)
            .map_err(|_| Error::CardsCorrupt(format!("invalid lapses {lapses}")))?,
    })
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::{Rating, apply_answer};

    #[test]
    fn apply_answer_updates_last_review_each_step() {
        // A fresh `apply_answer` call must overwrite `last_review`,
        // otherwise future replays would compute `days_elapsed` from
        // the very first answer instead of the most recent one.
        let t1 = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let t2 = t1 + chrono::Duration::days(30);

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
        // Same quiz answered three times. Each FSRS step must see the
        // gap to its *immediately preceding* answer, not to the original.
        // We prove it by showing that a three-step chain (gaps 30, 60)
        // ends in a different state than a two-step chain that skips the
        // middle answer (gap 90). If `apply_answer` ever used `ts - first`
        // for `days_elapsed`, the third step of the three-step chain
        // would collapse into the second step of the two-step chain.
        let t1 = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let t2 = t1 + chrono::Duration::days(30);
        let t3 = t2 + chrono::Duration::days(60);

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
}
