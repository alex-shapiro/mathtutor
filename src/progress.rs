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

/// Lesson and quiz status of an atom on a path.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct AtomStatus {
    pub lesson_taught: bool,
    pub complete: bool,
    pub easy: QuizSlot,
    pub medium: QuizSlot,
    pub hard: QuizSlot,
}

impl AtomStatus {
    /// Return the status of atom `c` in `progress`.
    pub fn of(progress: &PathProgress, c: &FlatConcept) -> Self {
        let slot = |difficulty| progress.quiz_slot(c, difficulty);
        Self {
            lesson_taught: progress.lesson_taught(&c.id),
            complete: scheduler::atom_action(c, progress).is_none(),
            easy: slot(Difficulty::Easy),
            medium: slot(Difficulty::Medium),
            hard: slot(Difficulty::Hard),
        }
    }

    /// Format the `[LEMH]` badge of this status.
    pub fn badge(&self) -> String {
        let slot = |s: QuizSlot, upper: char| match s {
            QuizSlot::Missing => '·',
            QuizSlot::Authored => upper.to_ascii_lowercase(),
            QuizSlot::Correct => upper,
        };
        let lesson = if self.lesson_taught { 'L' } else { '·' };
        format!(
            "[{lesson}{}{}{}]",
            slot(self.easy, 'E'),
            slot(self.medium, 'M'),
            slot(self.hard, 'H')
        )
    }
}

/// Status of a difficulty slot of an atom on a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuizSlot {
    /// The atom lacks a quiz of this difficulty.
    Missing,
    /// The atom has a quiz of this difficulty without a passing answer.
    Authored,
    /// A quiz of this difficulty has an answer rated `hard`, `good`, or `easy`.
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
    /// Count the completion of `target_atoms` and of the `reachable` atoms in `g`.
    pub fn of(
        g: &Graph,
        target_atoms: &[String],
        reachable: &[String],
        progress: &PathProgress,
    ) -> Self {
        let complete = |a: &String| scheduler::is_atom_complete(g, progress, a);
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
