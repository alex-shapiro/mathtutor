//! Per-path progress tracking for taught lessons and passed quizzes.

use std::collections::HashSet;

use serde::Serialize;

use crate::graph::Graph;
use crate::scheduler;

/// Taught atoms and passed quizzes for a learning path.
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
}

/// Completion counts for the target atoms of a path.
#[derive(Debug, Serialize)]
pub struct TargetProgress {
    pub total: usize,
    pub learned: usize,
    pub learned_pct: usize,
}

/// Completion counts across the atoms that the targets of a path reach.
#[derive(Debug, Serialize)]
pub struct ReachProgress {
    pub total: usize,
    pub taught: usize,
    pub learned: usize,
}

/// Completion counts of a path over its target atoms and the atoms that they reach.
#[derive(Debug)]
pub struct PathCompletion {
    pub targets: TargetProgress,
    pub reachable: ReachProgress,
}

impl PathCompletion {
    /// Count the completion of `target_atoms` and the atoms that they reach in `g`.
    pub fn of(g: &Graph, target_atoms: &[String], progress: &PathProgress) -> Self {
        let complete = |a: &String| scheduler::is_atom_complete(g, progress, a);
        let total = target_atoms.len();
        let learned = target_atoms.iter().filter(|a| complete(a)).count();
        let reachable = g.reachable_atoms(target_atoms);
        Self {
            targets: TargetProgress {
                total,
                learned,
                learned_pct: (learned * 100).checked_div(total).unwrap_or(0),
            },
            reachable: ReachProgress {
                total: reachable.len(),
                taught: reachable
                    .iter()
                    .filter(|a| progress.lesson_taught(a))
                    .count(),
                learned: reachable.iter().filter(|a| complete(a)).count(),
            },
        }
    }
}
