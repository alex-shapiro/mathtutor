//! FSRS spaced-repetition card state derived from path events.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use fsrs::{FSRS, MemoryState};

use crate::event_log::{Event, EventKind};
use crate::progress::PathProgress;
use crate::schema::{CardRecord, TaughtRow};
use crate::types::Rating;
use crate::{Error, Result};

const DESIRED_RETENTION: f32 = 0.9;

/// Maximum number of recent ratings retained per card, ordered newest first.
pub const RECENT: usize = 10;

/// FSRS memory and scheduling state for an answered quiz card.
#[derive(Debug, Clone, Copy)]
pub struct CardState {
    pub due: DateTime<Utc>,
    pub last_review: DateTime<Utc>,
    pub stability: f32,
    pub difficulty: f32,
}

#[derive(Debug, Clone)]
pub struct CardRow {
    pub path_id: String,
    pub quiz_id: String,
    pub state: Option<CardState>,
    pub reps: u32,
    pub lapses: u32,
    pub presented: u32,
    pub last_presented_at: Option<DateTime<Utc>>,
    pub first_correct_at: Option<DateTime<Utc>>,
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

/// Compute the next FSRS card state for `rating` at `ts`, with a minimum interval of 60 seconds.
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

/// `cards` and `taught` rows of a path that its events produce.
#[derive(Debug, Clone, Default)]
pub struct Projection {
    /// Card of each presented or answered quiz, keyed by quiz ID.
    pub cards: HashMap<String, CardRow>,
    /// Lesson presentations of each taught or authored atom, keyed by atom ID.
    pub taught: HashMap<String, Taught>,
}

impl Projection {
    /// Return the rows that `events` produce.
    pub fn from_events(events: &[Event]) -> Result<Self> {
        let mut out = Self::default();
        for e in events {
            out.apply(e, None)?;
        }
        Ok(out)
    }

    /// Apply event `e` to the row that it changes, and start a row that this projection lacks from `base`.
    pub fn apply(&mut self, e: &Event, base: Option<&Projection>) -> Result<()> {
        match &e.kind {
            EventKind::LessonTaught { atom } | EventKind::LessonAuthored { atom } => {
                let t = self.taught.entry(atom.clone()).or_insert_with(|| {
                    base.and_then(|b| b.taught.get(atom).cloned())
                        .unwrap_or_default()
                });
                if matches!(e.kind, EventKind::LessonTaught { .. }) {
                    t.count += 1;
                    t.last_at = Some(e.ts);
                }
            }
            EventKind::QuizPresented { quiz, .. } | EventKind::QuizAnswered { quiz, .. } => {
                let c = self.cards.entry(quiz.clone()).or_insert_with(|| {
                    base.and_then(|b| b.cards.get(quiz).cloned())
                        .unwrap_or_else(|| CardRow::new(&e.path, quiz))
                });
                apply_card_event(c, e)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Return the taught atoms and the passed quizzes of these rows.
    pub fn progress(&self) -> PathProgress {
        PathProgress {
            taught_atoms: self.taught.keys().cloned().collect(),
            correct_quizzes: self
                .cards
                .values()
                .filter(|c| c.reps > c.lapses)
                .map(|c| c.quiz_id.clone())
                .collect(),
        }
    }
}

/// Presentation count and last presentation timestamp for a lesson on a path.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Taught {
    pub count: u32,
    pub last_at: Option<DateTime<Utc>>,
}

impl Taught {
    pub fn into_row(self, path_id: &str, atom_id: String) -> TaughtRow {
        TaughtRow {
            path_id: path_id.to_string(),
            atom_id,
            count: self.count,
            last_at: self.last_at,
        }
    }
}

fn apply_card_event(c: &mut CardRow, e: &Event) -> Result<()> {
    match &e.kind {
        EventKind::QuizPresented { .. } => {
            c.presented += 1;
            c.last_presented_at = Some(e.ts);
        }
        &EventKind::QuizAnswered { rating, .. } => {
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

impl From<&CardRow> for CardRecord {
    fn from(c: &CardRow) -> Self {
        Self {
            path_id: c.path_id.clone(),
            quiz_id: c.quiz_id.clone(),
            stability: c.state.map(|s| s.stability),
            difficulty: c.state.map(|s| s.difficulty),
            due_at: c.state.map(|s| s.due),
            last_reviewed_at: c.state.map(|s| s.last_review),
            reps: c.reps,
            lapses: c.lapses,
            presented: c.presented,
            last_presented_at: c.last_presented_at,
            first_correct_at: c.first_correct_at,
            recent: c.recent.clone(),
        }
    }
}

impl TryFrom<CardRecord> for CardRow {
    type Error = Error;

    fn try_from(r: CardRecord) -> Result<Self> {
        let bad =
            |what: &str| Error::CardsCorrupt(format!("{what} in {}/{}", r.path_id, r.quiz_id));
        let state = match (r.stability, r.difficulty, r.due_at, r.last_reviewed_at) {
            (Some(stability), Some(difficulty), Some(due), Some(last_review)) => Some(CardState {
                due,
                last_review,
                stability,
                difficulty,
            }),
            (None, None, None, None) => None,
            _ => return Err(bad("partial FSRS state")),
        };
        if r.lapses > r.reps {
            return Err(bad("lapses > reps"));
        }
        Ok(Self {
            path_id: r.path_id,
            quiz_id: r.quiz_id,
            state,
            reps: r.reps,
            lapses: r.lapses,
            presented: r.presented,
            last_presented_at: r.last_presented_at,
            first_correct_at: r.first_correct_at,
            recent: r.recent,
        })
    }
}
