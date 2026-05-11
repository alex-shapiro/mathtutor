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
use crate::path::{PathError, path_dir, resolve_id};
use crate::types::{Difficulty, QuizType};

#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    #[error(transparent)]
    Path(#[from] PathError),
    #[error("io: {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("ayml serialize: {0}")]
    Serialize(String),
    #[error("ayml parse: {path}: {message}")]
    Parse { path: PathBuf, message: String },
}

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

pub fn overlay_path(path_id: &str) -> Result<PathBuf, PathError> {
    Ok(path_dir(path_id)?.join("overlay.ayml"))
}

pub fn load(path_id: &str) -> Result<Overlay, OverlayError> {
    let file_path = overlay_path(path_id)?;
    if !file_path.exists() {
        return Ok(Overlay {
            schema_version: 1,
            atoms: BTreeMap::new(),
        });
    }
    let file = File::open(&file_path).map_err(|e| OverlayError::Io {
        path: file_path.clone(),
        source: e,
    })?;
    if file
        .metadata()
        .map_err(|e| OverlayError::Io {
            path: file_path.clone(),
            source: e,
        })?
        .len()
        == 0
    {
        return Ok(Overlay {
            schema_version: 1,
            atoms: BTreeMap::new(),
        });
    }
    ayml::from_reader(BufReader::new(file)).map_err(|e| OverlayError::Parse {
        path: file_path,
        message: e.to_string(),
    })
}

pub fn save(path_id: &str, overlay: &Overlay) -> Result<(), OverlayError> {
    let file_path = overlay_path(path_id)?;
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent).map_err(|e| OverlayError::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let text = ayml::to_string(overlay).map_err(|e| OverlayError::Serialize(e.to_string()))?;
    fs::write(&file_path, text).map_err(|e| OverlayError::Io {
        path: file_path,
        source: e,
    })?;
    Ok(())
}

// ── Mutators used by `mt store …` (task #9) ────────────────────────

/// Set the lesson body for `atom_id` in this path's overlay. Returns an
/// error if the overlay already has a lesson for this atom — callers
/// should pre-check shipped-graph lesson presence too.
pub fn add_lesson(path_id: &str, atom_id: &str, body: String) -> Result<(), OverlayError> {
    let mut overlay = load(path_id)?;
    let entry = overlay
        .atoms
        .entry(atom_id.to_string())
        .or_insert_with(OverlayAtom::default);
    if entry.lesson.is_some() {
        return Err(OverlayError::Serialize(format!(
            "overlay already has a lesson for atom '{atom_id}'"
        )));
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
) -> Result<(), OverlayError> {
    let mut overlay = load(path_id)?;
    let entry = overlay
        .atoms
        .entry(atom_id.to_string())
        .or_insert_with(OverlayAtom::default);
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

pub fn cmd_dump(path_id: Option<&str>) -> Result<(), OverlayError> {
    let id = resolve_id(path_id)?;
    let overlay = load(&id)?;
    let text = ayml::to_string(&overlay).map_err(|e| OverlayError::Serialize(e.to_string()))?;
    print!("{text}");
    Ok(())
}
