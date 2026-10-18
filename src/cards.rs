//! FSRS card state.
//!
//! A card is the derived state of one quiz on one path: its FSRS memory
//! state plus the answer history `mt path next` reports. Cards are stored
//! in the `cards` table, updated by every `quiz_presented` and
//! `quiz_answered` event in the event's transaction. The event log
//! remains the source of truth; [`recompute`] rebuilds a path's cards
//! by replaying it.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration, Utc};
use fsrs::{FSRS, MemoryState};
use serde_json::{Value as Json, json};

use crate::db::{self, Db};
use crate::event_log::{self, Event, EventKind};
use crate::schema;
use crate::types::Rating;
use crate::view::Taught;
use crate::{Error, Result};

const DESIRED_RETENTION: f32 = 0.9;

/// Ratings a card remembers, newest first.
pub const RECENT: usize = 10;

/// FSRS state for a single quiz card. The fields exist precisely when a
/// card has been answered at least once. `apply_answer(None, …)` is the
/// first step that produces a state, so every field is always populated.
#[derive(Debug, Clone, Copy)]
pub struct CardState {
    pub due: DateTime<Utc>,
    pub last_review: DateTime<Utc>,
    pub stability: f32,
    pub difficulty: f32,
}

/// One quiz's card on one path.
#[derive(Debug, Clone)]
pub struct CardRow {
    pub path_id: String,
    pub quiz_id: String,
    /// `None` until the quiz is first answered.
    pub state: Option<CardState>,
    pub reps: u32,
    pub lapses: u32,
    pub presented: u32,
    pub last_presented_at: Option<DateTime<Utc>>,
    /// When the quiz was first answered correctly.
    pub first_correct_at: Option<DateTime<Utc>>,
    /// The latest ratings, newest first, at most [`RECENT`].
    pub recent: Vec<Rating>,
}

impl CardRow {
    pub fn new(path_id: &str, quiz_id: &str) -> Self {
        Self {
            path_id: path_id.to_string(),
            quiz_id: quiz_id.to_string(),
            state: None,
            reps: 0,
            lapses: 0,
            presented: 0,
            last_presented_at: None,
            first_correct_at: None,
            recent: Vec::new(),
        }
    }
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

// ── Events ─────────────────────────────────────────────────────────

/// Fold a `quiz_presented` or `quiz_answered` event into its card.
pub fn apply_event(c: &mut CardRow, e: &Event) -> Result<()> {
    match (e.kind, e.payload.rating) {
        (EventKind::QuizPresented, _) => {
            c.presented += 1;
            c.last_presented_at = Some(e.ts);
        }
        (EventKind::QuizAnswered, Some(rating)) => {
            c.state = Some(apply_answer(c.state.as_ref(), rating, e.ts)?);
            c.reps += 1;
            if rating == Rating::Again {
                c.lapses += 1;
            } else if c.first_correct_at.is_none() {
                c.first_correct_at = Some(e.ts);
            }
            c.recent.insert(0, rating);
            c.recent.truncate(RECENT);
        }
        _ => {}
    }
    Ok(())
}

// ── Rows ───────────────────────────────────────────────────────────

/// The op that stores `c`.
pub fn upsert_op(c: &CardRow) -> Json {
    let ts = |t: Option<DateTime<Utc>>| t.map(db::format_ts);
    json!({ "op": "upsert_row", "table": "cards", "row": {
        "path_id": c.path_id,
        "quiz_id": c.quiz_id,
        "stability": c.state.map(|s| f64::from(s.stability)),
        "difficulty": c.state.map(|s| f64::from(s.difficulty)),
        "due_at": ts(c.state.map(|s| s.due)),
        "last_reviewed_at": ts(c.state.map(|s| s.last_review)),
        "reps": c.reps,
        "lapses": c.lapses,
        "presented": c.presented,
        "last_presented_at": ts(c.last_presented_at),
        "first_correct_at": ts(c.first_correct_at),
        "recent": c.recent.iter().map(|r| i64::from(*r)).collect::<Vec<_>>(),
    }})
}

/// The card a `cards` row holds.
#[allow(clippy::cast_possible_truncation)]
pub fn decode(r: &Json) -> Result<CardRow> {
    let bad = |k: &str| Error::CardsCorrupt(format!("{k} in {r}"));
    let s = |k: &str| r[k].as_str().ok_or_else(|| bad(k)).map(str::to_string);
    let n = |k: &str| {
        r[k].as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| bad(k))
    };
    let ts = |k: &str| r[k].as_str().map(db::parse_ts).transpose();
    let state = match (
        r["stability"].as_f64(),
        r["difficulty"].as_f64(),
        ts("due_at")?,
        ts("last_reviewed_at")?,
    ) {
        (Some(stability), Some(difficulty), Some(due), Some(last_review)) => Some(CardState {
            due,
            last_review,
            stability: stability as f32,
            difficulty: difficulty as f32,
        }),
        (None, None, None, None) => None,
        _ => return Err(bad("FSRS state")),
    };
    let recent = r["recent"]
        .as_array()
        .ok_or_else(|| bad("recent"))?
        .iter()
        .map(|v| {
            v.as_i64()
                .ok_or_else(|| bad("recent"))
                .and_then(Rating::try_from)
        })
        .collect::<Result<Vec<_>>>()?;
    let (reps, lapses) = (n("reps")?, n("lapses")?);
    if lapses > reps {
        return Err(bad("lapses > reps"));
    }
    Ok(CardRow {
        path_id: s("path_id")?,
        quiz_id: s("quiz_id")?,
        state,
        reps,
        lapses,
        presented: n("presented")?,
        last_presented_at: ts("last_presented_at")?,
        first_correct_at: ts("first_correct_at")?,
        recent,
    })
}

// ── Rebuild ────────────────────────────────────────────────────────

/// Replace `path_id`'s cards and taught lessons with the state its event
/// log implies. Use after a suspected corruption or a change to how
/// events are folded.
///
/// The log is read and the rows rewritten in one transaction, so an event
/// logged concurrently either lands before the replay or conflicts with it.
pub async fn recompute(db: &Db, path_id: &str) -> Result<()> {
    db::retry(|| recompute_once(db, path_id)).await
}

async fn recompute_once(db: &Db, path_id: &str) -> Result<()> {
    let stream = schema::events_stream(path_id);
    let read = |from: u64| json!({ "op": "read_stream", "stream": stream, "from_seq": from, "limit": 1000 });
    let (txn, mut r) = db.begin(&[read(0)]).await?;
    let mut events = Vec::new();
    loop {
        let page = r.swap_remove(0);
        for rec in page["records"].as_array().into_iter().flatten() {
            events.push(event_log::decode(&rec["json"])?);
        }
        if page["done"] != false {
            break;
        }
        let from = page["next_seq"]
            .as_u64()
            .ok_or_else(|| Error::BadResponse(format!("next_seq: {page}")))?;
        r = txn.ops(&[read(from)]).await?;
    }
    let (cards, taught) = fold_history(path_id, &events)?;
    let mut ops = vec![
        json!({ "op": "delete_rows", "table": "cards", "where": { "eq": [path_id] } }),
        json!({ "op": "delete_rows", "table": "taught", "where": { "eq": [path_id] } }),
    ];
    ops.extend(cards.iter().map(upsert_op));
    ops.extend(taught.into_iter().map(|(atom_id, t)| {
        json!({ "op": "upsert_row", "table": "taught", "row": {
            "path_id": path_id, "atom_id": atom_id, "count": t.count,
            "last_at": t.last_at.map(db::format_ts),
        }})
    }));
    txn.commit(&ops).await?;
    Ok(())
}

/// Fold a path's events into its cards and taught lessons.
pub fn fold_history(
    path_id: &str,
    events: &[Event],
) -> Result<(Vec<CardRow>, BTreeMap<String, Taught>)> {
    let mut cards: HashMap<String, CardRow> = HashMap::new();
    let mut taught: BTreeMap<String, Taught> = BTreeMap::new();
    for e in events {
        match (e.kind, e.quiz.as_deref(), e.atom.as_deref()) {
            (EventKind::QuizPresented | EventKind::QuizAnswered, Some(quiz), _) => {
                let c = cards
                    .entry(quiz.to_string())
                    .or_insert_with(|| CardRow::new(path_id, quiz));
                apply_event(c, e)?;
            }
            (EventKind::LessonTaught | EventKind::LessonAuthored, _, Some(atom)) => {
                let t = taught.entry(atom.to_string()).or_default();
                if e.kind == EventKind::LessonTaught {
                    t.count += 1;
                    t.last_at = Some(e.ts);
                }
            }
            _ => {}
        }
    }
    let mut cards: Vec<CardRow> = cards.into_values().collect();
    cards.sort_by(|a, b| a.quiz_id.cmp(&b.quiz_id));
    Ok((cards, taught))
}
