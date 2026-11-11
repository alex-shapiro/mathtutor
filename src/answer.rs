use crate::Result;
use crate::db::Db;
use crate::event_log::{Event, EventKind};

use crate::types::Rating;
use crate::view::Want;

/// Extract the atom ID prefix from a quiz ID, such as `fnd.1.1.1` from `fnd.1.1.1.q3`.
pub fn atom_from_quiz_id(quiz_id: &str) -> Option<String> {
    let pos = quiz_id.rfind(".q")?;
    Some(quiz_id[..pos].to_string())
}

pub async fn cmd_quiz_answer(
    db: &Db,
    quiz_id: &str,
    rating: Rating,
    user_answer: Option<String>,
    path_id: Option<&str>,
) -> Result<()> {
    db.transact(&Want::path(path_id), |view, changes| {
        let id = view.path()?.file.id.clone();
        // Verify that the quiz exists in the merged graph before recording an event.
        let g = view.graph()?;
        let (atom, _) = g.quiz(quiz_id)?;
        let answered = EventKind::QuizAnswered {
            atom: Some(atom.id.clone()),
            quiz: quiz_id.to_string(),
            rating,
            user_answer: user_answer.clone(),
        };
        changes.append(view, &Event::now(id, answered))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::atom_from_quiz_id;

    #[test]
    fn atom_from_quiz_id_strips_q_suffix() {
        assert_eq!(
            atom_from_quiz_id("fnd.1.1.1.q3").as_deref(),
            Some("fnd.1.1.1")
        );
        assert_eq!(atom_from_quiz_id("no-dot-q-here"), None);
    }
}
