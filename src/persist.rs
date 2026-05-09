//! Write authored content (lessons, quizzes) back into the canonical curriculum graph.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::event_log;
use crate::graph::{self, AreaFileRaw, LeafRaw, Manifest, NodeRaw, TopicRaw};
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
    #[error("schema mismatch: file claims schema_version=1 but has no `topics:`")]
    WrongSchemaV1,
    #[error("schema mismatch: file claims schema_version=2 but has no `children:`")]
    WrongSchemaV2,
    #[error("unknown schema_version: {0}")]
    UnknownSchema(u32),
}

// ── Public command ─────────────────────────────────────────────────

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
    })?;

    Ok(())
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

    let text = ayml::to_string(&raw).map_err(|e| PersistError::Serialize(e.to_string()))?;
    fs::write(&area_path, text).map_err(|e| PersistError::Io {
        path: area_path.clone(),
        source: e,
    })?;
    Ok(())
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

/// Two-pass mutable lookup: locate by ID via read-only DFS to record the
/// index path, then walk that path with sequential mutable borrows. This
/// avoids the borrow-checker conflict from recursive `&mut` returns.
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

// `AreaFileRaw` is used through `&mut raw` above; make sure the type stays in scope.
#[allow(dead_code)]
fn _ensure_area_file_raw_in_scope(_: &AreaFileRaw) {}
