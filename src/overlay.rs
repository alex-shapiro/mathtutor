//! User-authored lesson and quiz overrides for the curriculum.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::db::{Db, Op};
use crate::graph::Quiz;
use crate::schema::{LessonRow, QuizRow, RemovedQuizRow};
use crate::types::{Difficulty, QuizType};
use crate::view::{Changes, Want};
use crate::{Error, Result};

/// In-memory snapshot of the curriculum overlay.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Overlay {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub atoms: BTreeMap<String, OverlayAtom>,
}

/// Overlay state for a single atom.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct OverlayAtom {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lesson: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quizzes: Vec<Quiz>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub removed: BTreeSet<String>,
}

impl Overlay {
    /// Build an overlay from database rows.
    pub fn from_rows(
        lessons: Vec<LessonRow>,
        quizzes: Vec<QuizRow>,
        removed: Vec<RemovedQuizRow>,
    ) -> Self {
        let mut atoms: BTreeMap<String, OverlayAtom> = BTreeMap::new();
        for r in lessons {
            atoms.entry(r.atom_id).or_default().lesson = Some(r.body);
        }
        for r in quizzes {
            let quiz = Quiz {
                id: r.quiz_id,
                difficulty: r.difficulty,
                kind: r.kind,
                question: r.question,
                answer: r.answer,
                rubric: r.rubric,
            };
            atoms.entry(r.atom_id).or_default().quizzes.push(quiz);
        }
        for r in removed {
            atoms
                .entry(r.atom_id)
                .or_default()
                .removed
                .insert(r.quiz_id);
        }
        Self { atoms }
    }
}

impl QuizRow {
    pub fn new(atom_id: &str, q: &Quiz) -> Self {
        Self {
            quiz_id: q.id.clone(),
            atom_id: atom_id.to_string(),
            difficulty: q.difficulty,
            kind: q.kind,
            question: q.question.clone(),
            answer: q.answer.clone(),
            rubric: q.rubric.clone(),
        }
    }
}

/// Mutation applied to the curriculum overlay.
#[derive(Debug, Clone)]
pub enum Edit {
    Lesson {
        atom_id: String,
        body: String,
    },
    Quiz {
        atom_id: String,
        quiz: Quiz,
        new: bool,
    },
    Remove {
        atom_id: String,
        quiz_id: String,
    },
}

impl Edit {
    /// Convert this edit into database write operations.
    pub fn ops(&self) -> Vec<Op> {
        match self {
            Edit::Lesson { atom_id, body } => vec![Op::upsert(&LessonRow {
                atom_id: atom_id.clone(),
                body: body.clone(),
            })],
            // Use `insert` for new quizzes so concurrent transactions cannot claim the same ID.
            Edit::Quiz {
                atom_id,
                quiz,
                new: true,
            } => vec![Op::insert(&QuizRow::new(atom_id, quiz))],
            Edit::Quiz {
                atom_id,
                quiz,
                new: false,
            } => vec![
                Op::upsert(&QuizRow::new(atom_id, quiz)),
                Op::delete_row::<RemovedQuizRow>(&[&quiz.id]),
            ],
            Edit::Remove { atom_id, quiz_id } => vec![Op::upsert(&RemovedQuizRow {
                quiz_id: quiz_id.clone(),
                atom_id: atom_id.clone(),
            })],
        }
    }
}

impl Overlay {
    /// Apply `e` to the in-memory overlay.
    pub fn apply(&mut self, e: &Edit) {
        match e {
            Edit::Lesson { atom_id, body } => {
                self.atoms.entry(atom_id.clone()).or_default().lesson = Some(body.clone());
            }
            Edit::Quiz { atom_id, quiz, .. } => {
                let a = self.atoms.entry(atom_id.clone()).or_default();
                if let Some(q) = a.quizzes.iter_mut().find(|q| q.id == quiz.id) {
                    q.clone_from(quiz);
                } else {
                    a.quizzes.push(quiz.clone());
                    a.quizzes.sort_by(|x, y| x.id.cmp(&y.id));
                }
                a.removed.remove(&quiz.id);
            }
            Edit::Remove { atom_id, quiz_id } => {
                self.atoms
                    .entry(atom_id.clone())
                    .or_default()
                    .removed
                    .insert(quiz_id.clone());
            }
        }
    }
}

/// Set the lesson body for `atom_id` in the overlay.
pub fn upsert_lesson(changes: &mut Changes, atom_id: &str, body: &str) {
    changes.overlay(Edit::Lesson {
        atom_id: atom_id.into(),
        body: body.into(),
    });
}

/// Insert a new quiz into the overlay.
pub fn add_quiz(changes: &mut Changes, atom_id: &str, q: &Quiz) {
    changes.overlay(Edit::Quiz {
        atom_id: atom_id.into(),
        quiz: q.clone(),
        new: true,
    });
}

/// Update fields of `base` in the overlay and clear any removal tombstone.
#[allow(clippy::too_many_arguments)]
pub fn amend_quiz(
    changes: &mut Changes,
    atom_id: &str,
    base: &Quiz,
    new_difficulty: Option<Difficulty>,
    new_question: Option<&str>,
    new_answer: Option<&str>,
    new_rubric: Option<&str>,
    new_type: Option<QuizType>,
) {
    let quiz = Quiz {
        id: base.id.clone(),
        difficulty: new_difficulty.unwrap_or(base.difficulty),
        kind: match new_type {
            Some(t) => (t != QuizType::FreeText).then_some(t),
            None => base.kind,
        },
        question: new_question.unwrap_or(&base.question).to_string(),
        answer: new_answer.unwrap_or(&base.answer).to_string(),
        rubric: new_rubric.or(base.rubric.as_deref()).map(str::to_string),
    };
    changes.overlay(Edit::Quiz {
        atom_id: atom_id.into(),
        quiz,
        new: false,
    });
}

/// Mark `quiz_id` as removed in the overlay.
pub fn remove_quiz(changes: &mut Changes, atom_id: &str, quiz_id: &str) {
    changes.overlay(Edit::Remove {
        atom_id: atom_id.into(),
        quiz_id: quiz_id.into(),
    });
}

/// Print the curriculum overlay as AYML (`mt graph dump`).
pub async fn cmd_graph_dump(db: &Db) -> Result<()> {
    let overlay = db.snapshot(&Want::overlay()).await?.overlay;
    let text = ayml::to_string(&*overlay).map_err(|e| Error::AymlSerialize(e.to_string()))?;
    print!("{text}");
    Ok(())
}
