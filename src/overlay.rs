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

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::BufReader;
use std::path::PathBuf;

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quizzes: Vec<QuizRaw>,
}

impl OverlayAtom {
    pub fn is_empty(&self) -> bool {
        self.lesson.is_none() && self.quizzes.is_empty()
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

// ── `mt overlay dump` ──────────────────────────────────────────────

pub fn cmd_dump(path_id: Option<&str>) -> Result<()> {
    let id = resolve_id(path_id)?;
    let overlay = load(&id)?;
    let text = ayml::to_string(&overlay).map_err(|e| Error::AymlSerialize(e.to_string()))?;
    print!("{text}");
    Ok(())
}
