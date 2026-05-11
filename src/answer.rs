//! `mt answer`: record a quiz answer as a `quiz_answered` event.
//!
//! FSRS card state is derived from the event log on demand (see
//! `crate::cards`), so this command's job is just to log the answer.

use crate::Result;
use crate::event_log;
use crate::path;
use crate::types::Rating;

pub fn cmd_answer(
    quiz_id: &str,
    rating: Rating,
    user_answer: Option<String>,
    path_id: Option<&str>,
) -> Result<()> {
    let id = path::resolve_id(path_id)?;
    event_log::append(event_log::quiz_answered(
        id,
        atom_from_quiz_id(quiz_id),
        quiz_id.to_string(),
        rating,
        user_answer,
    ))
}

/// Recover the atom ID from a quiz ID like `fnd.1.1.1.q3`.
pub fn atom_from_quiz_id(quiz_id: &str) -> Option<String> {
    let pos = quiz_id.rfind(".q")?;
    Some(quiz_id[..pos].to_string())
}
