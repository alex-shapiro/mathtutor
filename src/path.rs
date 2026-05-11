//! Learning-path data, per-path storage, and `mt new`.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::event_log;
use crate::graph::{self, Graph};

#[derive(Debug, thiserror::Error)]
pub enum PathError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("ayml serialize: {0}")]
    Serialize(String),
    #[error("ayml parse: {path} {msg}")]
    Parse { path: String, msg: String },
    #[error("graph: {0}")]
    Graph(#[from] graph::LoadError),
    #[error("unknown id: {0}")]
    UnknownId(String),
    #[error("cluster '{0}' has no atomic descendants")]
    EmptyCluster(String),
    #[error("cycle in target atoms")]
    Cycle,
    #[error("no learning path found (run `mt new` first)")]
    NoPath,
    #[error("HOME not set; set MATHTUTOR_HOME or HOME")]
    NoHome,
}

/// Immutable record of the learner's goal for a path. Written once at
/// `mt new`; never updated. All mutable per-path state (quiz answers,
/// FSRS card state, lessons taught) lives in the event log; FSRS state
/// is derived from the log on demand via `crate::cards`.
#[derive(Debug, Serialize, Deserialize)]
pub struct PathFile {
    pub schema_version: u32,
    pub id: String,
    pub goal: String,
    pub created_at: DateTime<Utc>,
    pub target_atoms: Vec<String>,
}

// ── Storage layout ──────────────────────────────────────────────────

pub fn mt_home() -> Result<PathBuf, PathError> {
    if let Ok(p) = std::env::var("MATHTUTOR_HOME") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var("HOME").map_err(|_| PathError::NoHome)?;
    Ok(PathBuf::from(home).join(".mathtutor"))
}

pub fn paths_root() -> Result<PathBuf, PathError> {
    Ok(mt_home()?.join("paths"))
}

pub fn path_dir(id: &str) -> Result<PathBuf, PathError> {
    Ok(paths_root()?.join(id))
}

pub fn save_path(p: &PathFile) -> Result<(), PathError> {
    let dir = path_dir(&p.id)?;
    fs::create_dir_all(&dir)?;
    let text = ayml::to_string(p).map_err(|e| PathError::Serialize(e.to_string()))?;
    fs::write(dir.join("path.ayml"), text)?;
    Ok(())
}

pub fn load_path(id: &str) -> Result<PathFile, PathError> {
    let path = path_dir(id)?.join("path.ayml");
    let file = File::open(&path)?;
    ayml::from_reader(BufReader::new(file)).map_err(|e| PathError::Parse {
        path: path.to_string_lossy().into(),
        msg: e.to_string(),
    })
}

pub fn most_recent_id() -> Result<Option<String>, PathError> {
    let root = paths_root()?;
    if !root.exists() {
        return Ok(None);
    }
    let mut entries: Vec<_> = fs::read_dir(&root)?
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.metadata().and_then(|m| m.modified()).ok()));
    Ok(entries
        .into_iter()
        .next()
        .and_then(|e| e.file_name().to_str().map(String::from)))
}

pub fn resolve_id(explicit: Option<&str>) -> Result<String, PathError> {
    if let Some(id) = explicit {
        return Ok(id.to_string());
    }
    most_recent_id()?.ok_or(PathError::NoPath)
}

pub fn generate_path_id(now: DateTime<Utc>) -> String {
    format!("p_{}", now.format("%Y_%m_%d_%H%M%S"))
}

// ── Commands ────────────────────────────────────────────────────────

pub fn cmd_new(goal: &str, ids: &[String], graph_dir: Option<&Path>) -> Result<String, PathError> {
    let g = Graph::load_default(graph_dir)?;
    let expanded = expand_to_atoms(&g, ids)?;
    let sorted = topo_sort(&g, &expanded)?;

    let now = Utc::now();
    let id = generate_path_id(now);

    let p = PathFile {
        schema_version: 1,
        id: id.clone(),
        goal: goal.to_string(),
        created_at: now,
        target_atoms: sorted,
    };

    save_path(&p)?;

    event_log::append(event_log::path_created(id.clone()))?;

    Ok(id)
}

/// Expand each input ID into a deduplicated set of atom IDs.
///
/// - An atom (leaf node) is included as-is.
/// - A cluster (non-leaf node) is expanded to all atomic descendants.
/// - A bare area prefix (e.g. `tx`) — not itself a node, since the
///   graph's roots are topic-level (e.g. `tx.1`) — is expanded to all
///   atoms whose ID starts with `<prefix>.`.
fn expand_to_atoms(g: &Graph, ids: &[String]) -> Result<Vec<String>, PathError> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();

    for id in ids {
        let before = out.len();
        match g.by_id.get(id) {
            Some(c) if c.children_ids.is_empty() => {
                if seen.insert(id.clone()) {
                    out.push(id.clone());
                }
            }
            Some(_) => {
                collect_atomic_descendants(g, id, &mut seen, &mut out);
                if out.len() == before {
                    return Err(PathError::EmptyCluster(id.clone()));
                }
            }
            None if !id.contains('.') => {
                collect_atoms_by_prefix(g, id, &mut seen, &mut out);
                if out.len() == before {
                    return Err(PathError::UnknownId(id.clone()));
                }
            }
            None => return Err(PathError::UnknownId(id.clone())),
        }
    }
    Ok(out)
}

fn collect_atomic_descendants(
    g: &Graph,
    id: &str,
    seen: &mut HashSet<String>,
    out: &mut Vec<String>,
) {
    let Some(c) = g.by_id.get(id) else { return };
    if c.children_ids.is_empty() {
        if seen.insert(id.to_string()) {
            out.push(id.to_string());
        }
    } else {
        for child in &c.children_ids {
            collect_atomic_descendants(g, child, seen, out);
        }
    }
}

fn collect_atoms_by_prefix(
    g: &Graph,
    prefix: &str,
    seen: &mut HashSet<String>,
    out: &mut Vec<String>,
) {
    let prefix_dot = format!("{prefix}.");
    let mut matched: Vec<String> = g
        .by_id
        .iter()
        .filter(|(id, c)| id.starts_with(&prefix_dot) && c.children_ids.is_empty())
        .map(|(id, _)| id.clone())
        .collect();
    matched.sort();
    for id in matched {
        if seen.insert(id.clone()) {
            out.push(id);
        }
    }
}

// ── Topological sort over the user-supplied target atoms ───────────

fn topo_sort(g: &Graph, atoms: &[String]) -> Result<Vec<String>, PathError> {
    let atom_set: HashSet<&str> = atoms.iter().map(String::as_str).collect();
    let mut indegree: HashMap<&str, usize> = atoms.iter().map(|a| (a.as_str(), 0)).collect();
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();

    for atom in atoms {
        if let Some(c) = g.by_id.get(atom) {
            for p in &c.prerequisites {
                if atom_set.contains(p.as_str()) {
                    adj.entry(p.as_str()).or_default().push(atom.as_str());
                    *indegree.get_mut(atom.as_str()).unwrap() += 1;
                }
            }
        }
    }

    // Stable: sort the initial frontier so output is deterministic.
    let mut frontier: Vec<&str> = indegree
        .iter()
        .filter_map(|(k, &v)| if v == 0 { Some(*k) } else { None })
        .collect();
    frontier.sort_unstable();

    let mut result = Vec::new();
    while !frontier.is_empty() {
        let node = frontier.remove(0);
        result.push(node.to_string());
        if let Some(neighbors) = adj.get(node) {
            let mut neighbors = neighbors.clone();
            neighbors.sort_unstable();
            for n in neighbors {
                let entry = indegree.get_mut(n).unwrap();
                *entry -= 1;
                if *entry == 0 {
                    frontier.push(n);
                    frontier.sort_unstable();
                }
            }
        }
    }

    if result.len() != atoms.len() {
        return Err(PathError::Cycle);
    }
    Ok(result)
}
