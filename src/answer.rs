use std::path::Path;

use crate::Result;
use crate::db::Db;
use crate::event_log;

use crate::types::Rating;
use crate::view::Want;

/// Extracts the atom ID prefix from a quiz ID (for example, `fnd.1.1.1.q3` -> `fnd.1.1.1`).
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
    graph_dir: Option<&Path>,
) -> Result<()> {
    db.transact(&Want::path(path_id), |view, changes| {
        let id = view.path()?.file.id.clone();
        // Verify that the quiz exists in the merged graph before recording an event.
        let g = view.graph(graph_dir)?;
        let (atom, _) = g.quiz(quiz_id)?;
        let e = event_log::quiz_answered(
            id,
            Some(atom.id.clone()),
            quiz_id.to_string(),
            rating,
            user_answer.clone(),
        );
        changes.append(view, &e)
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
