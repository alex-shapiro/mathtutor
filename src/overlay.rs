//! User-authored lesson and quiz overrides for the curriculum.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::db::{Db, Op};
use crate::graph::Quiz;
use crate::schema::{LessonRow, QuizRow, RemovedQuizRow};
use crate::types::{Difficulty, QuizType};
use crate::view::{Changes, Want};

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

/// Stored rows of the overlay, keyed by primary key.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct OverlayRows {
    lessons: BTreeMap<String, LessonRow>,
    quizzes: BTreeMap<String, QuizRow>,
    removed: BTreeMap<String, RemovedQuizRow>,
}

impl OverlayRows {
    /// Collect the rows that a read of the overlay tables returned.
    pub fn new(
        lessons: Vec<LessonRow>,
        quizzes: Vec<QuizRow>,
        removed: Vec<RemovedQuizRow>,
    ) -> Self {
        Self {
            lessons: lessons
                .into_iter()
                .map(|r| (r.atom_id.clone(), r))
                .collect(),
            quizzes: quizzes
                .into_iter()
                .map(|r| (r.quiz_id.clone(), r))
                .collect(),
            removed: removed
                .into_iter()
                .map(|r| (r.quiz_id.clone(), r))
                .collect(),
        }
    }

    /// Apply `change` as the database applies its operation.
    pub(crate) fn apply(&mut self, change: RowChange) {
        match change {
            RowChange::PutLesson(r) => {
                self.lessons.insert(r.atom_id.clone(), r);
            }
            RowChange::InsertQuiz(r) | RowChange::PutQuiz(r) => {
                self.quizzes.insert(r.quiz_id.clone(), r);
            }
            RowChange::PutRemoved(r) => {
                self.removed.insert(r.quiz_id.clone(), r);
            }
            RowChange::DeleteRemoved(quiz_id) => {
                self.removed.remove(&quiz_id);
            }
        }
    }

    /// Return the overlay that these rows store.
    pub fn overlay(&self) -> Overlay {
        let mut atoms: BTreeMap<String, OverlayAtom> = BTreeMap::new();
        for r in self.lessons.values() {
            atoms.entry(r.atom_id.clone()).or_default().lesson = Some(r.body.clone());
        }
        for r in self.quizzes.values() {
            let quiz = Quiz {
                id: r.quiz_id.clone(),
                difficulty: r.difficulty,
                kind: r.kind,
                question: r.question.clone(),
                answer: r.answer.clone(),
                rubric: r.rubric.clone(),
            };
            atoms
                .entry(r.atom_id.clone())
                .or_default()
                .quizzes
                .push(quiz);
        }
        for r in self.removed.values() {
            atoms
                .entry(r.atom_id.clone())
                .or_default()
                .removed
                .insert(r.quiz_id.clone());
        }
        Overlay { atoms }
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

/// Change of one overlay row.
#[derive(Debug, Clone)]
pub(crate) enum RowChange {
    PutLesson(LessonRow),
    /// Insert a quiz row, which fails if a row with the same ID exists, so that concurrent transactions cannot claim the same ID.
    InsertQuiz(QuizRow),
    PutQuiz(QuizRow),
    PutRemoved(RemovedQuizRow),
    /// Delete the removal tombstone of a quiz ID.
    DeleteRemoved(String),
}

impl RowChange {
    /// Return the database write operation of this change.
    pub(crate) fn op(&self) -> Op {
        match self {
            RowChange::PutLesson(r) => Op::upsert(r),
            RowChange::InsertQuiz(r) => Op::insert(r),
            RowChange::PutQuiz(r) => Op::upsert(r),
            RowChange::PutRemoved(r) => Op::upsert(r),
            RowChange::DeleteRemoved(quiz_id) => Op::delete_row::<RemovedQuizRow>(quiz_id),
        }
    }
}

impl Edit {
    /// Return the row changes of this edit.
    pub(crate) fn row_changes(&self) -> Vec<RowChange> {
        match self {
            Edit::Lesson { atom_id, body } => vec![RowChange::PutLesson(LessonRow {
                atom_id: atom_id.clone(),
                body: body.clone(),
            })],
            Edit::Quiz {
                atom_id,
                quiz,
                new: true,
            } => vec![RowChange::InsertQuiz(QuizRow::new(atom_id, quiz))],
            Edit::Quiz {
                atom_id,
                quiz,
                new: false,
            } => vec![
                RowChange::PutQuiz(QuizRow::new(atom_id, quiz)),
                RowChange::DeleteRemoved(quiz.id.clone()),
            ],
            Edit::Remove { atom_id, quiz_id } => vec![RowChange::PutRemoved(RemovedQuizRow {
                quiz_id: quiz_id.clone(),
                atom_id: atom_id.clone(),
            })],
        }
    }
}

/// Fields of a quiz that an agent authors.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
pub struct QuizDraft {
    /// Difficulty slot (`easy`, `medium`, or `hard`).
    pub difficulty: Difficulty,
    /// Question text.
    pub question: String,
    /// Reference answer text.
    pub answer: String,
    /// Optional grading rubric.
    #[serde(default)]
    pub rubric: Option<String>,
    /// Quiz format (`free_text` or `multiple_choice`, default `free_text`).
    #[serde(default)]
    pub kind: QuizType,
}

impl QuizDraft {
    /// Return the quiz with these fields and ID `id`.
    pub fn into_quiz(self, id: String) -> Quiz {
        Quiz {
            id,
            difficulty: self.difficulty,
            kind: (self.kind != QuizType::FreeText).then_some(self.kind),
            question: self.question,
            answer: self.answer,
            rubric: self.rubric,
        }
    }
}

/// Fields of an existing quiz to replace. An omitted field keeps its current value.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
pub struct QuizPatch {
    /// Updated difficulty slot (`easy`, `medium`, or `hard`).
    #[serde(default)]
    pub difficulty: Option<Difficulty>,
    /// Updated question text.
    #[serde(default)]
    pub question: Option<String>,
    /// Updated reference answer text.
    #[serde(default)]
    pub answer: Option<String>,
    /// Updated grading rubric.
    #[serde(default)]
    pub rubric: Option<String>,
    /// Updated quiz format (`free_text` or `multiple_choice`).
    #[serde(default)]
    pub kind: Option<QuizType>,
}

impl QuizPatch {
    /// Return `base` with the fields of this patch replaced.
    pub fn apply(&self, base: &Quiz) -> Quiz {
        Quiz {
            id: base.id.clone(),
            difficulty: self.difficulty.unwrap_or(base.difficulty),
            kind: match self.kind {
                Some(t) => (t != QuizType::FreeText).then_some(t),
                None => base.kind,
            },
            question: self
                .question
                .clone()
                .unwrap_or_else(|| base.question.clone()),
            answer: self.answer.clone().unwrap_or_else(|| base.answer.clone()),
            rubric: self.rubric.clone().or_else(|| base.rubric.clone()),
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

/// Replace `base` in the overlay with `patch` applied, and clear any removal tombstone.
pub fn amend_quiz(changes: &mut Changes, atom_id: &str, base: &Quiz, patch: &QuizPatch) {
    changes.overlay(Edit::Quiz {
        atom_id: atom_id.into(),
        quiz: patch.apply(base),
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
    let view = db.snapshot(&Want::overlay()).await?;
    crate::print_ayml(view.overlay()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Quiz {
        Quiz {
            id: "a.q1".into(),
            difficulty: Difficulty::Easy,
            kind: Some(QuizType::MultipleChoice),
            question: "q".into(),
            answer: "a".into(),
            rubric: Some("r".into()),
        }
    }

    #[test]
    fn patch_replaces_only_the_fields_that_it_sets() {
        let patch = QuizPatch {
            question: Some("new".into()),
            ..QuizPatch::default()
        };
        let q = patch.apply(&base());
        assert_eq!(q.question, "new");
        assert_eq!(
            (q.id.as_str(), q.answer.as_str(), q.difficulty),
            ("a.q1", "a", Difficulty::Easy)
        );
        assert_eq!(q.kind, Some(QuizType::MultipleChoice));
        assert_eq!(q.rubric.as_deref(), Some("r"));
    }

    #[test]
    fn free_text_is_the_unstored_default_kind() {
        let patch = QuizPatch {
            kind: Some(QuizType::FreeText),
            ..QuizPatch::default()
        };
        assert_eq!(patch.apply(&base()).kind, None);
        let draft: QuizDraft =
            serde_json::from_str(r#"{"difficulty":"hard","question":"q","answer":"a"}"#).unwrap();
        let q = draft.into_quiz("b.q1".into());
        assert_eq!((q.kind, q.difficulty), (None, Difficulty::Hard));
    }
}
