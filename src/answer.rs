//! `mt answer`: record a quiz answer as an FSRS rating, update the card
//! state in `path.ayml`, and log a `quiz_answered` event.

use std::path::Path;

use chrono::{Duration, Utc};
use fsrs::{FSRS, MemoryState};

use crate::event_log;
use crate::path::{self, CardState, PathError};
use crate::types::Rating;

const DESIRED_RETENTION: f32 = 0.9;

#[derive(Debug, thiserror::Error)]
pub enum AnswerError {
    #[error(transparent)]
    Path(#[from] PathError),
    #[error("fsrs: {0}")]
    Fsrs(String),
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
pub fn cmd_answer(
    quiz_id: &str,
    rating: Rating,
    path_id: Option<&str>,
    _graph_dir: &Path,
) -> Result<(), AnswerError> {
    let id = path::resolve_id(path_id)?;
    let mut p = path::load_path(&id)?;
    let now = Utc::now();

    let prev = p.cards.get(quiz_id).cloned();
    let days_elapsed = match prev.as_ref().and_then(|c| c.last_review) {
        Some(lr) => (now - lr).num_days().max(0) as u32,
        None => 0,
    };
    let memory = prev
        .as_ref()
        .and_then(|c| match (c.stability, c.difficulty) {
            (Some(s), Some(d)) => Some(MemoryState {
                stability: s,
                difficulty: d,
            }),
            _ => None,
        });

    let fsrs = FSRS::new(Some(&[])).map_err(|e| AnswerError::Fsrs(format!("{e:?}")))?;
    let next_states = fsrs
        .next_states(memory, DESIRED_RETENTION, days_elapsed)
        .map_err(|e| AnswerError::Fsrs(format!("{e:?}")))?;

    let next = match rating {
        Rating::Again => next_states.again,
        Rating::Hard => next_states.hard,
        Rating::Good => next_states.good,
        Rating::Easy => next_states.easy,
    };

    // FSRS returns the interval in fractional days; we keep it as seconds
    // so sub-day relearning intervals retain their precision. The 60-second
    // floor prevents zero-interval scheduling.
    let interval_secs = (next.interval * 86_400.0).round().max(60.0) as i64;
    let due = now + Duration::seconds(interval_secs);

    p.cards.insert(
        quiz_id.to_string(),
        CardState {
            due,
            last_review: Some(now),
            stability: Some(next.memory.stability),
            difficulty: Some(next.memory.difficulty),
            last_rating: Some(rating),
        },
    );

    path::save_path(&p)?;

    event_log::append(event_log::quiz_answered(
        id,
        atom_from_quiz_id(quiz_id),
        quiz_id.to_string(),
        rating,
    ))?;

    Ok(())
}

/// Recover the atom ID from a quiz ID like `fnd.1.1.1.q3`.
pub fn atom_from_quiz_id(quiz_id: &str) -> Option<String> {
    let pos = quiz_id.rfind(".q")?;
    Some(quiz_id[..pos].to_string())
}
