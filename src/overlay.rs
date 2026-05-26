//! Per-path overlay: lessons and quizzes a user has authored on top of
//! the shipped curriculum. Lives at `~/.mathtutor/paths/<id>/overlay.ayml`.
//!
//! The shipped curriculum is read-only (compiled into the binary). When
//! a user authors a new lesson or quiz, it goes into the active path's
//! overlay instead of mutating the canonical graph. `Graph::load_for_path`
//! merges the overlay onto the shipped data to produce the effective
//! graph for that path's scheduler / tree / state queries.
//!
//! Blast radius is per-path on purpose: an unaudited lesson authored
//! under path A doesn't pollute path B that touches the same atom.
//! `mt overlay dump` prints a path's overlay for review and eventual
//! merge into the canonical curriculum.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::BufReader;
use std::path::PathBuf;

use libsql::Connection;
use serde::{Deserialize, Serialize};

use crate::graph::{Quiz, QuizRaw};
use crate::path::{path_dir, resolve_id};
use crate::types::{Difficulty, QuizType};
use crate::{Error, Result};

/// On-disk shape of `overlay.ayml`. Flat: atoms keyed by ID, each
/// carrying the lesson and/or quizzes this path has authored. No
/// cluster structure — overlays only carry content, not topology.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Overlay {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub atoms: BTreeMap<String, OverlayAtom>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct OverlayAtom {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lesson: Option<String>,
    /// Authored quizzes for this atom. An entry whose id matches a
    /// shipped quiz id overrides the shipped version during merge;
    /// otherwise it's a new quiz appended after the shipped ones.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quizzes: Vec<QuizRaw>,
    /// Quiz ids that should not appear in the merged view, whether
    /// they originated in the shipped curriculum or the overlay's own
    /// `quizzes`. The `QuizAnswered` events for these ids remain in the
    /// log for audit; the scheduler simply stops surfacing them.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub removed: BTreeSet<String>,
}

impl OverlayAtom {
    pub fn is_empty(&self) -> bool {
        self.lesson.is_none() && self.quizzes.is_empty() && self.removed.is_empty()
    }

    /// Quizzes in `FlatConcept` form, for merge into the shipped graph.
    pub fn quizzes_flat(&self) -> Vec<Quiz> {
        self.quizzes.iter().cloned().map(Quiz::from).collect()
    }
}

// ── Storage layout ──────────────────────────────────────────────────

pub fn overlay_path(path_id: &str) -> Result<PathBuf> {
    Ok(path_dir(path_id)?.join("overlay.ayml"))
}

pub fn load(path_id: &str) -> Result<Overlay> {
    let file_path = overlay_path(path_id)?;
    if !file_path.exists() {
        return Ok(Overlay {
            schema_version: 1,
            atoms: BTreeMap::new(),
        });
    }
    let file = File::open(&file_path).map_err(|e| Error::Io {
        path: file_path.clone(),
        source: e,
    })?;
    let metadata = file.metadata().map_err(|e| Error::Io {
        path: file_path.clone(),
        source: e,
    })?;
    if metadata.len() == 0 {
        return Ok(Overlay {
            schema_version: 1,
            atoms: BTreeMap::new(),
        });
    }
    ayml::from_reader(BufReader::new(file)).map_err(|e| Error::AymlParse {
        path: file_path,
        message: e.to_string(),
    })
}

pub fn save(path_id: &str, overlay: &Overlay) -> Result<()> {
    let file_path = overlay_path(path_id)?;
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let text = ayml::to_string(overlay).map_err(|e| Error::AymlSerialize(e.to_string()))?;
    fs::write(&file_path, text).map_err(|e| Error::Io {
        path: file_path,
        source: e,
    })
}

// ── Mutators used by `mt store …` ──────────────────────────────────

/// Set the lesson body for `atom_id` in this path's overlay. Returns
/// `Error::LessonAlreadyExists` if the overlay already has one;
/// callers should pre-check shipped-graph lesson presence too.
pub fn add_lesson(path_id: &str, atom_id: &str, body: String) -> Result<()> {
    let mut overlay = load(path_id)?;
    let entry = overlay.atoms.entry(atom_id.to_string()).or_default();
    if entry.lesson.is_some() {
        return Err(Error::LessonAlreadyExists(atom_id.to_string()));
    }
    entry.lesson = Some(body);
    save(path_id, &overlay)
}

/// Append a new quiz to `atom_id` in this path's overlay. The caller
/// supplies a unique quiz id (typically derived from the highest
/// existing quiz id across shipped + overlay).
#[allow(clippy::too_many_arguments)]
pub fn add_quiz(
    path_id: &str,
    atom_id: &str,
    quiz_id: String,
    difficulty: Difficulty,
    question: String,
    answer: String,
    rubric: Option<String>,
    quiz_type: QuizType,
) -> Result<()> {
    let mut overlay = load(path_id)?;
    let entry = overlay.atoms.entry(atom_id.to_string()).or_default();
    let kind = (quiz_type != QuizType::FreeText).then_some(quiz_type);
    entry.quizzes.push(QuizRaw {
        id: quiz_id,
        difficulty,
        kind,
        question,
        answer,
        rubric,
    });
    save(path_id, &overlay)
}

/// Apply field changes to an existing quiz in this path's overlay.
/// If the quiz currently lives in the shipped curriculum (no overlay
/// entry yet), the overlay gains a new entry that shadows it; if the
/// quiz is already overlay-authored, that entry is mutated in place.
///
/// `base` is the quiz's current state in the *merged* view, supplied
/// by the caller (typically `store::cmd_amend_quiz` looked it up via
/// `Graph::load_for_path`). Only fields supplied here change.
#[allow(clippy::too_many_arguments)]
pub fn amend_quiz(
    path_id: &str,
    atom_id: &str,
    base: &QuizRaw,
    new_difficulty: Option<Difficulty>,
    new_question: Option<String>,
    new_answer: Option<String>,
    new_rubric: Option<String>,
    new_type: Option<QuizType>,
) -> Result<()> {
    let updated = QuizRaw {
        id: base.id.clone(),
        difficulty: new_difficulty.unwrap_or(base.difficulty),
        kind: match new_type {
            Some(t) => (t != QuizType::FreeText).then_some(t),
            None => base.kind,
        },
        question: new_question.unwrap_or_else(|| base.question.clone()),
        answer: new_answer.unwrap_or_else(|| base.answer.clone()),
        rubric: new_rubric.or_else(|| base.rubric.clone()),
    };

    let mut overlay = load(path_id)?;
    let entry = overlay.atoms.entry(atom_id.to_string()).or_default();
    match entry.quizzes.iter_mut().find(|q| q.id == updated.id) {
        Some(existing) => *existing = updated,
        None => entry.quizzes.push(updated),
    }
    // Amending un-tombstones, in case the quiz had been removed.
    entry.removed.remove(&base.id);
    save(path_id, &overlay)
}

/// Tombstone a quiz id in this path's overlay so it stops appearing in
/// the merged view. Idempotent.
pub fn remove_quiz(path_id: &str, atom_id: &str, quiz_id: &str) -> Result<()> {
    let mut overlay = load(path_id)?;
    let entry = overlay.atoms.entry(atom_id.to_string()).or_default();
    entry.removed.insert(quiz_id.to_string());
    save(path_id, &overlay)
}

// ── `mt overlay dump` ──────────────────────────────────────────────

pub async fn cmd_dump(conn: &Connection, path_id: Option<&str>) -> Result<()> {
    let id = resolve_id(conn, path_id).await?;
    let overlay = load(&id)?;
    let text = ayml::to_string(&overlay).map_err(|e| Error::AymlSerialize(e.to_string()))?;
    print!("{text}");
    Ok(())
}
