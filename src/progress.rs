//! Per-path progress tracking for taught lessons and passed quizzes.

use std::collections::HashSet;

use serde::Serialize;

use crate::graph::{FlatConcept, Graph};
use crate::scheduler;
use crate::types::Difficulty;

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

    /// Return the status of the `difficulty` slot of atom `c`.
    pub fn quiz_slot(&self, c: &FlatConcept, difficulty: Difficulty) -> QuizSlot {
        let mut quizzes = c
            .quizzes
            .iter()
            .filter(|q| q.difficulty == difficulty)
            .peekable();
        if quizzes.peek().is_none() {
            QuizSlot::Missing
        } else if quizzes.any(|q| self.quiz_answered_correctly(&q.id)) {
            QuizSlot::Correct
        } else {
            QuizSlot::Authored
        }
    }
}

/// Status of a difficulty slot of an atom on a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuizSlot {
    /// The atom has no quiz of the difficulty.
    Missing,
    /// The atom has a quiz of the difficulty, but no quiz of the difficulty has a passing answer.
    Authored,
    /// A quiz of the difficulty has an answer rated `hard`, `good`, or `easy`.
    Correct,
}

/// Completion counts for the target atoms of a path.
#[derive(Debug, Serialize)]
pub struct TargetProgress {
    pub total: usize,
    pub learned: usize,
    pub learned_pct: usize,
}

impl TargetProgress {
    /// Count the completed atoms of `target_atoms` in `g`.
    pub fn of(g: &Graph, target_atoms: &[String], progress: &PathProgress) -> Self {
        let total = target_atoms.len();
        let learned = target_atoms
            .iter()
            .filter(|a| scheduler::is_atom_complete(g, progress, a))
            .count();
        Self {
            total,
            learned,
            learned_pct: (learned * 100).checked_div(total).unwrap_or(0),
        }
    }
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
        let reachable = g.reachable_atoms(target_atoms);
        Self {
            targets: TargetProgress::of(g, target_atoms, progress),
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
