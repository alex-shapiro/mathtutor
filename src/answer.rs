//! `mt answer`: record a quiz answer.

use std::path::Path;

use libsql::Connection;

use crate::event_log;
use crate::graph::Graph;
use crate::path;
use crate::types::Rating;
use crate::{Error, Result};

/// Recover the atom ID from a quiz ID like `fnd.1.1.1.q3`.
pub fn atom_from_quiz_id(quiz_id: &str) -> Option<String> {
    let pos = quiz_id.rfind(".q")?;
    Some(quiz_id[..pos].to_string())
}

/// Verify `quiz_id` resolves to a real atom and a quiz that atom owns
/// in the merged graph, and return the parent atom id. Pure — no I/O —
/// so the validation contract can be exercised with in-memory fixtures.
pub fn resolve_quiz(g: &Graph, quiz_id: &str) -> Result<String> {
    let atom_id =
        atom_from_quiz_id(quiz_id).ok_or_else(|| Error::UnknownId(quiz_id.to_string()))?;
    let atom = g
        .by_id
        .get(&atom_id)
        .ok_or_else(|| Error::AtomNotFound(atom_id.clone()))?;
    if !atom.quizzes.iter().any(|q| q.id == quiz_id) {
        return Err(Error::UnknownId(quiz_id.to_string()));
    }
    Ok(atom_id)
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
    let g = Graph::load_for_path(&id, graph_dir)?;
    // Validate the quiz exists in the merged graph before writing
    // anything — otherwise a typo silently leaves an event row and a
    // ghost `cards` entry for a quiz no scheduler will ever surface.
    let atom_id = resolve_quiz(&g, quiz_id)?;

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
    use std::collections::HashMap;

    use super::{atom_from_quiz_id, resolve_quiz};
    use crate::Error;
    use crate::graph::{FlatConcept, Graph, Quiz};
    use crate::types::Difficulty;

    fn graph_with_quiz(atom_id: &str, quiz_id: &str) -> Graph {
        let mut by_id = HashMap::new();
        by_id.insert(
            atom_id.to_string(),
            FlatConcept {
                id: atom_id.into(),
                name: atom_id.into(),
                description: None,
                prerequisites: Vec::new(),
                children_ids: Vec::new(),
                lesson: Some("body".into()),
                quizzes: vec![Quiz {
                    id: quiz_id.into(),
                    difficulty: Difficulty::Easy,
                    kind: None,
                    question: "q".into(),
                    answer: "a".into(),
                    rubric: None,
                }],
            },
        );
        Graph { by_id }
    }

    #[test]
    fn atom_from_quiz_id_strips_q_suffix() {
        assert_eq!(
            atom_from_quiz_id("fnd.1.1.1.q3").as_deref(),
            Some("fnd.1.1.1")
        );
        assert_eq!(atom_from_quiz_id("no-dot-q-here"), None);
    }

    #[test]
    fn resolve_quiz_accepts_real_quiz() {
        let g = graph_with_quiz("fnd.1.1.1", "fnd.1.1.1.q1");
        let atom_id = resolve_quiz(&g, "fnd.1.1.1.q1").expect("valid");
        assert_eq!(atom_id, "fnd.1.1.1");
    }

    #[test]
    fn resolve_quiz_rejects_malformed_id() {
        let g = graph_with_quiz("fnd.1.1.1", "fnd.1.1.1.q1");
        // Missing `.qN` suffix → can't derive an atom id.
        assert!(matches!(
            resolve_quiz(&g, "fnd.1.1.1"),
            Err(Error::UnknownId(_))
        ));
    }

    #[test]
    fn resolve_quiz_rejects_unknown_atom() {
        let g = graph_with_quiz("fnd.1.1.1", "fnd.1.1.1.q1");
        assert!(matches!(
            resolve_quiz(&g, "nope.1.q1"),
            Err(Error::AtomNotFound(_))
        ));
    }

    #[test]
    fn resolve_quiz_rejects_unknown_quiz_on_known_atom() {
        // Atom is real but doesn't own a `.q9` quiz — the most likely
        // typo path (right atom, wrong index).
        let g = graph_with_quiz("fnd.1.1.1", "fnd.1.1.1.q1");
        assert!(matches!(
            resolve_quiz(&g, "fnd.1.1.1.q9"),
            Err(Error::UnknownId(_))
        ));
    }
}
