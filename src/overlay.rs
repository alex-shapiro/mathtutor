//! User-authored lesson and quiz overrides for the curriculum.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::db::{Db, Op};
use crate::graph::Quiz;
use crate::schema::{LessonRow, QuizRow, RemovedQuizRow};
use crate::types::{Difficulty, QuizType};
use crate::view::{View, ViewRequest, WriteSet};

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

impl OverlayAtom {
    fn is_empty(&self) -> bool {
        self.lesson.is_none() && self.quizzes.is_empty() && self.removed.is_empty()
    }
}

impl Overlay {
    /// Return the overlay that the rows of the overlay tables store.
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
            let atom = atoms.entry(r.atom_id.clone()).or_default();
            atom.quizzes.push(Quiz::from(r));
        }
        for r in removed {
            atoms
                .entry(r.atom_id)
                .or_default()
                .removed
                .insert(r.quiz_id);
        }
        for atom in atoms.values_mut() {
            atom.quizzes.sort_by(|a, b| a.id.cmp(&b.id));
        }
        Overlay { atoms }
    }

    /// Apply `change` as the database applies its operation to the overlay rows.
    pub(crate) fn apply(&mut self, change: RowChange) {
        match change {
            RowChange::PutLesson(r) => {
                self.atoms.entry(r.atom_id).or_default().lesson = Some(r.body);
            }
            RowChange::InsertQuiz(r) | RowChange::PutQuiz(r) => {
                self.edit_atoms(|a| a.quizzes.retain(|q| q.id != r.quiz_id));
                let quizzes = &mut self.atoms.entry(r.atom_id.clone()).or_default().quizzes;
                let at = quizzes.partition_point(|q| q.id < r.quiz_id);
                quizzes.insert(at, Quiz::from(r));
            }
            RowChange::PutRemoved(r) => {
                self.edit_atoms(|a| {
                    a.removed.remove(&r.quiz_id);
                });
                self.atoms
                    .entry(r.atom_id)
                    .or_default()
                    .removed
                    .insert(r.quiz_id);
            }
        }
    }

    /// Apply `edit` to every atom, and drop each atom that the edit leaves empty.
    fn edit_atoms(&mut self, mut edit: impl FnMut(&mut OverlayAtom)) {
        self.atoms.retain(|_, a| {
            edit(a);
            !a.is_empty()
        });
    }
}

impl From<QuizRow> for Quiz {
    fn from(r: QuizRow) -> Self {
        Quiz {
            id: r.quiz_id,
            difficulty: r.difficulty,
            kind: r.kind,
            question: r.question,
            answer: r.answer,
            rubric: r.rubric,
        }
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

/// Change of a single overlay row.
#[derive(Debug, Clone)]
pub enum RowChange {
    PutLesson(LessonRow),
    /// Insert a quiz row, which fails if a row with the same ID exists, so that concurrent transactions cannot claim the same ID.
    InsertQuiz(QuizRow),
    PutQuiz(QuizRow),
    PutRemoved(RemovedQuizRow),
}

impl RowChange {
    /// Return the database write operation of this change.
    pub(crate) fn op(&self) -> Op {
        match self {
            RowChange::PutLesson(r) => Op::upsert(r),
            RowChange::InsertQuiz(r) => Op::insert(r),
            RowChange::PutQuiz(r) => Op::upsert(r),
            RowChange::PutRemoved(r) => Op::upsert(r),
        }
    }
}

/// Fields of a quiz that an agent authors.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
pub struct QuizDraft {
    /// Difficulty slot: `easy`, `medium`, or `hard`.
    pub difficulty: Difficulty,
    /// Question text.
    pub question: String,
    /// Reference answer text.
    pub answer: String,
    /// Optional grading rubric.
    #[serde(default)]
    pub rubric: Option<String>,
    /// Quiz format: `free_text` or `multiple_choice`. Defaults to `free_text`.
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
    /// Updated difficulty slot: `easy`, `medium`, or `hard`.
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
    /// Updated quiz format: `free_text` or `multiple_choice`.
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

/// Set the lesson body for `atom_id` in the overlay that `view` loaded.
pub fn upsert_lesson(changes: &mut WriteSet, view: &View, atom_id: &str, body: &str) -> Result<()> {
    let row = LessonRow {
        atom_id: atom_id.into(),
        body: body.into(),
    };
    changes.overlay(view, RowChange::PutLesson(row))
}

/// Insert a new quiz into the overlay that `view` loaded.
pub fn add_quiz(changes: &mut WriteSet, view: &View, atom_id: &str, q: &Quiz) -> Result<()> {
    changes.overlay(view, RowChange::InsertQuiz(QuizRow::new(atom_id, q)))
}

/// Replace `base` in the overlay that `view` loaded with `patch` applied.
pub fn amend_quiz(
    changes: &mut WriteSet,
    view: &View,
    atom_id: &str,
    base: &Quiz,
    patch: &QuizPatch,
) -> Result<()> {
    let row = QuizRow::new(atom_id, &patch.apply(base));
    changes.overlay(view, RowChange::PutQuiz(row))
}

/// Mark `quiz_id` as removed in the overlay that `view` loaded.
pub fn remove_quiz(
    changes: &mut WriteSet,
    view: &View,
    atom_id: &str,
    quiz_id: &str,
) -> Result<()> {
    let row = RemovedQuizRow {
        quiz_id: quiz_id.into(),
        atom_id: atom_id.into(),
    };
    changes.overlay(view, RowChange::PutRemoved(row))
}

/// Print the curriculum overlay as AYML for `mt graph dump`.
pub async fn cmd_graph_dump(db: &Db) -> Result<()> {
    let view = db.snapshot(&ViewRequest::overlay()).await?;
    crate::print_ayml(view.overlay()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiz_row(atom: &str, id: &str) -> QuizRow {
        QuizRow::new(
            atom,
            &Quiz {
                id: id.into(),
                ..base()
            },
        )
    }

    fn removed_row(atom: &str, id: &str) -> RemovedQuizRow {
        RemovedQuizRow {
            atom_id: atom.into(),
            quiz_id: id.into(),
        }
    }

    #[test]
    fn applied_changes_give_the_overlay_of_the_resulting_rows() {
        let lesson = LessonRow {
            atom_id: "a".into(),
            body: "b".into(),
        };
        let changes = [
            RowChange::PutLesson(lesson.clone()),
            RowChange::InsertQuiz(quiz_row("b", "q2")),
            RowChange::InsertQuiz(quiz_row("b", "q1")),
            RowChange::PutQuiz(quiz_row("c", "q2")),
            RowChange::PutRemoved(removed_row("d", "x")),
            RowChange::PutRemoved(removed_row("e", "x")),
        ];
        let mut applied = Overlay::default();
        for change in changes {
            applied.apply(change);
        }

        let stored = Overlay::from_rows(
            vec![lesson],
            vec![quiz_row("c", "q2"), quiz_row("b", "q1")],
            vec![removed_row("e", "x")],
        );
        assert_eq!(
            serde_json::to_value(&applied).unwrap(),
            serde_json::to_value(&stored).unwrap()
        );
        assert_eq!(
            applied.atoms.keys().collect::<Vec<_>>(),
            ["a", "b", "c", "e"]
        );
    }

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
