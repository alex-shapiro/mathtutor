//! `mt answer`: record a quiz answer.

use std::path::Path;

use libsql::Connection;

use crate::Result;
use crate::event_log;
use crate::graph::Graph;
use crate::path;
use crate::types::Rating;

/// Recover the atom ID from a quiz ID like `fnd.1.1.1.q3`.
pub fn atom_from_quiz_id(quiz_id: &str) -> Option<String> {
    let pos = quiz_id.rfind(".q")?;
    Some(quiz_id[..pos].to_string())
}

pub async fn cmd_answer(
    conn: &Connection,
    quiz_id: &str,
    rating: Rating,
    user_answer: Option<String>,
    path_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<()> {
    let tx = conn.transaction().await?;
    let id = path::resolve_id(&tx, path_id).await?;
    let g = Graph::load_for_path(&tx, graph_dir).await?;
    // Validate the quiz exists in the merged graph before writing
    // anything — otherwise a typo silently leaves an event row and a
    // ghost `cards` entry for a quiz no scheduler will ever surface.
    let (atom, _) = g.quiz(quiz_id)?;
    let atom_id = atom.id.clone();

    event_log::append(
        &tx,
        &event_log::quiz_answered(id, Some(atom_id), quiz_id.to_string(), rating, user_answer),
    )
    .await?;
    tx.commit().await?;
    Ok(())
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
