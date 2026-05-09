//! Write authored content (lessons, quizzes) back into the canonical curriculum graph.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::event_log;
use crate::graph::{
    self, AreaFileRaw, Difficulty, LeafRaw, Manifest, NodeRaw, QuizRaw, QuizType, TopicRaw,
};
use crate::path::{self, PathError};

#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    #[error("graph: {0}")]
    Graph(#[from] graph::LoadError),
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
    #[error("unknown atom-id prefix: '{0}' (no matching area)")]
    UnknownPrefix(String),
    #[error("atom '{0}' not found in graph")]
    AtomNotFound(String),
    #[error("'{0}' is a cluster, not an atom")]
    NotAtom(String),
    #[error("atom '{0}' already has a stored lesson; use `mt amend lesson` to replace")]
    LessonAlreadyExists(String),
    #[error("atom '{0}' has no stored lesson; teach it before authoring quizzes")]
    NoLesson(String),
    #[error("schema mismatch: file claims schema_version=1 but has no `topics:`")]
    WrongSchemaV1,
    #[error("schema mismatch: file claims schema_version=2 but has no `children:`")]
    WrongSchemaV2,
    #[error("unknown schema_version: {0}")]
    UnknownSchema(u32),
}

// ── Public commands ────────────────────────────────────────────────

/// Persist a lesson body for an atom into the canonical graph and log
/// `lesson_authored` against the active learning path.
pub fn cmd_store_lesson(
    atom_id: &str,
    body: String,
    path_id: Option<&str>,
    graph_dir: &Path,
) -> Result<(), PersistError> {
    store_lesson_in_graph(graph_dir, atom_id, body)?;

    let id = path::resolve_id(path_id)?;
    event_log::append(event_log::Event {
        ts: Utc::now(),
        kind: "lesson_authored".to_string(),
        path: id,
        atom: Some(atom_id.to_string()),
        quiz: None,
        payload: event_log::EventPayload::default(),
    })?;

    Ok(())
}

/// Persist a quiz on an atom (free-text by default), generating a stable
/// `<atom>.q<n>` ID. Logs `quiz_authored` against the active path.
/// Returns the generated quiz ID.
pub fn cmd_store_quiz(
    atom_id: &str,
    difficulty: Difficulty,
    question: String,
    answer: String,
    rubric: Option<String>,
    quiz_type: QuizType,
    path_id: Option<&str>,
    graph_dir: &Path,
) -> Result<String, PersistError> {
    let quiz_id = store_quiz_in_graph(
        graph_dir, atom_id, difficulty, question, answer, rubric, quiz_type,
    )?;

    let id = path::resolve_id(path_id)?;
    event_log::append(event_log::Event {
        ts: Utc::now(),
        kind: "quiz_authored".to_string(),
        path: id,
        atom: Some(atom_id.to_string()),
        quiz: Some(quiz_id.clone()),
        payload: event_log::EventPayload::default(),
    })?;

    Ok(quiz_id)
}

// ── Implementation ─────────────────────────────────────────────────

fn store_lesson_in_graph(
    graph_dir: &Path,
    atom_id: &str,
    body: String,
) -> Result<(), PersistError> {
    let manifest = graph::load_manifest(&graph_dir.join("manifest.ayml"))?;
    let area_path = area_file_for_atom(&manifest, graph_dir, atom_id)?;
    let mut raw = graph::load_area(&area_path)?;

    match raw.schema_version {
        1 => {
            let topics = raw.topics.as_mut().ok_or(PersistError::WrongSchemaV1)?;
            let leaf = find_leaf_mut(topics, atom_id)
                .ok_or_else(|| PersistError::AtomNotFound(atom_id.to_string()))?;
            if leaf.lesson.is_some() {
                return Err(PersistError::LessonAlreadyExists(atom_id.to_string()));
            }
            leaf.lesson = Some(body);
        }
        2 => {
            let children = raw.children.as_mut().ok_or(PersistError::WrongSchemaV2)?;
            let node = find_node_mut(children, atom_id)
                .ok_or_else(|| PersistError::AtomNotFound(atom_id.to_string()))?;
            let has_children = node.children.as_ref().is_some_and(|c| !c.is_empty());
            if has_children {
                return Err(PersistError::NotAtom(atom_id.to_string()));
            }
            if node.lesson.is_some() {
                return Err(PersistError::LessonAlreadyExists(atom_id.to_string()));
            }
            node.lesson = Some(body);
        }
        v => return Err(PersistError::UnknownSchema(v)),
    }

    write_area(&area_path, &raw)
}

#[allow(clippy::too_many_arguments)]
fn store_quiz_in_graph(
    graph_dir: &Path,
    atom_id: &str,
    difficulty: Difficulty,
    question: String,
    answer: String,
    rubric: Option<String>,
    quiz_type: QuizType,
) -> Result<String, PersistError> {
    // Skip the default type on disk so unannotated quizzes don't grow a
    // redundant `type: free_text` line.
    let kind = (quiz_type != QuizType::FreeText).then_some(quiz_type);

    let manifest = graph::load_manifest(&graph_dir.join("manifest.ayml"))?;
    let area_path = area_file_for_atom(&manifest, graph_dir, atom_id)?;
    let mut raw = graph::load_area(&area_path)?;

    let new_id = match raw.schema_version {
        1 => {
            let topics = raw.topics.as_mut().ok_or(PersistError::WrongSchemaV1)?;
            let leaf = find_leaf_mut(topics, atom_id)
                .ok_or_else(|| PersistError::AtomNotFound(atom_id.to_string()))?;
            if leaf.lesson.is_none() {
                return Err(PersistError::NoLesson(atom_id.to_string()));
            }
            let new_id = next_quiz_id(atom_id, leaf.quizzes.as_deref().unwrap_or(&[]));
            let q = QuizRaw {
                id: new_id.clone(),
                difficulty,
                kind,
                question,
                answer,
                rubric,
            };
            leaf.quizzes.get_or_insert_with(Vec::new).push(q);
            new_id
        }
        2 => {
            let children = raw.children.as_mut().ok_or(PersistError::WrongSchemaV2)?;
            let node = find_node_mut(children, atom_id)
                .ok_or_else(|| PersistError::AtomNotFound(atom_id.to_string()))?;
            let has_children = node.children.as_ref().is_some_and(|c| !c.is_empty());
            if has_children {
                return Err(PersistError::NotAtom(atom_id.to_string()));
            }
            if node.lesson.is_none() {
                return Err(PersistError::NoLesson(atom_id.to_string()));
            }
            let new_id = next_quiz_id(atom_id, node.quizzes.as_deref().unwrap_or(&[]));
            let q = QuizRaw {
                id: new_id.clone(),
                difficulty,
                kind,
                question,
                answer,
                rubric,
            };
            node.quizzes.get_or_insert_with(Vec::new).push(q);
            new_id
        }
        v => return Err(PersistError::UnknownSchema(v)),
    };

    write_area(&area_path, &raw)?;
    Ok(new_id)
}

fn write_area(path: &Path, raw: &AreaFileRaw) -> Result<(), PersistError> {
    let text = ayml::to_string(raw).map_err(|e| PersistError::Serialize(e.to_string()))?;
    fs::write(path, text).map_err(|e| PersistError::Io {
        path: path.to_path_buf(),
        source: e,
    })
}

fn area_file_for_atom(
    manifest: &Manifest,
    graph_dir: &Path,
    atom_id: &str,
) -> Result<PathBuf, PersistError> {
    let prefix = atom_id.split('.').next().unwrap_or("");
    let entry = manifest
        .areas
        .iter()
        .find(|e| e.prefix == prefix)
        .ok_or_else(|| PersistError::UnknownPrefix(prefix.to_string()))?;
    Ok(graph_dir.join(&entry.file))
}

/// Returns `<atom>.q<n>` where n is one greater than the highest
/// existing n. Quiz IDs are stable; gaps from deletions are never
/// reused.
fn next_quiz_id(atom_id: &str, existing: &[QuizRaw]) -> String {
    let prefix = format!("{atom_id}.q");
    let max = existing
        .iter()
        .filter_map(|q| q.id.strip_prefix(&prefix))
        .filter_map(|s| s.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    format!("{prefix}{}", max + 1)
}

// ── Mutable lookup helpers (read-only path probe + sequential descent) ──

fn find_node_mut<'a>(nodes: &'a mut [NodeRaw], id: &str) -> Option<&'a mut NodeRaw> {
    let path = locate_node_path(nodes, id)?;
    node_at_path(nodes, &path)
}

fn locate_node_path(nodes: &[NodeRaw], id: &str) -> Option<Vec<usize>> {
    let mut path = Vec::new();
    if locate_helper(nodes, id, &mut path) {
        Some(path)
    } else {
        None
    }
}

fn locate_helper(nodes: &[NodeRaw], id: &str, path: &mut Vec<usize>) -> bool {
    for (i, n) in nodes.iter().enumerate() {
        path.push(i);
        if n.id == id {
            return true;
        }
        if let Some(children) = &n.children
            && locate_helper(children, id, path)
        {
            return true;
        }
        path.pop();
    }
    false
}

fn node_at_path<'a>(nodes: &'a mut [NodeRaw], path: &[usize]) -> Option<&'a mut NodeRaw> {
    if path.is_empty() {
        return None;
    }
    let mut current: &mut [NodeRaw] = nodes;
    for &idx in &path[..path.len() - 1] {
        current = current.get_mut(idx)?.children.as_mut()?;
    }
    current.get_mut(*path.last().unwrap())
}

fn find_leaf_mut<'a>(topics: &'a mut [TopicRaw], id: &str) -> Option<&'a mut LeafRaw> {
    for t in topics.iter_mut() {
        for l in t.leaves.iter_mut() {
            if l.id == id {
                return Some(l);
            }
        }
    }
    None
}
