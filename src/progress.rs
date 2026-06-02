//! Per-path progress: atoms taught, quizzes answered correctly.

use std::collections::HashSet;

use libsql::Connection;

use crate::Result;
use crate::cards;

#[derive(Debug, Default, Clone)]
pub struct PathProgress {
    pub taught_atoms: HashSet<String>,
    pub correct_quizzes: HashSet<String>,
}

impl PathProgress {
    pub fn lesson_taught(&self, atom_id: &str) -> bool {
        self.taught_atoms.contains(atom_id)
    }

    pub fn quiz_answered_correctly(&self, quiz_id: &str) -> bool {
        self.correct_quizzes.contains(quiz_id)
    }

    /// Load a path's progress snapshot from its two backing tables.
    pub async fn load(conn: &Connection, path_id: &str) -> Result<Self> {
        Ok(Self {
            taught_atoms: load_taught_atoms(conn, path_id).await?,
            correct_quizzes: cards::correct_quiz_ids(conn, path_id).await?,
        })
    }
}

async fn load_taught_atoms(conn: &Connection, path_id: &str) -> Result<HashSet<String>> {
    let mut rows = conn
        .query(
            "SELECT DISTINCT atom_id FROM events \
             WHERE path_id = ? AND kind IN ('lesson_taught','lesson_authored') \
               AND atom_id IS NOT NULL",
            libsql::params![path_id],
        )
        .await?;
    let mut out = HashSet::new();
    while let Some(row) = rows.next().await? {
        out.insert(row.get(0)?);
    }
    Ok(out)
}
