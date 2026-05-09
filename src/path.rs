//! Learning-path data, per-path storage, and `mt new` / `mt state`.

use std::collections::{HashMap, HashSet};
use std::fs;
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
    #[error("ayml parse: {0}")]
    Parse(String),
    #[error("graph: {0}")]
    Graph(#[from] graph::LoadError),
    #[error("unknown atom id: {0}")]
    UnknownAtom(String),
    #[error("'{0}' is a cluster, not an atom — pick a leaf concept")]
    NotAtom(String),
    #[error("cycle in target atoms")]
    Cycle,
    #[error("no learning path found (run `mt new` first)")]
    NoPath,
    #[error("HOME not set; set MATHTUTOR_HOME or HOME")]
    NoHome,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PathFile {
    pub schema_version: u32,
    pub id: String,
    pub goal: String,
    pub created_at: DateTime<Utc>,
    pub target_atoms: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub cards: HashMap<String, CardState>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct CardState {
    pub repetitions: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_rating: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_presented_at: Option<DateTime<Utc>>,
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
    let dir = path_dir(id)?;
    let text = fs::read_to_string(dir.join("path.ayml"))?;
    ayml::from_str(&text).map_err(|e| PathError::Parse(e.to_string()))
}

pub fn most_recent_id() -> Result<Option<String>, PathError> {
    let root = paths_root()?;
    if !root.exists() {
        return Ok(None);
    }
    let mut entries: Vec<_> = fs::read_dir(&root)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.metadata().and_then(|m| m.modified()).ok()));
    Ok(entries
        .into_iter()
        .next()
        .and_then(|e| e.file_name().to_str().map(|s| s.to_string())))
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

pub fn cmd_new(goal: &str, atoms: &[String], graph_dir: &Path) -> Result<String, PathError> {
    let g = Graph::load(graph_dir)?;

    for a in atoms {
        let c = g
            .by_id
            .get(a)
            .ok_or_else(|| PathError::UnknownAtom(a.clone()))?;
        if !c.children_ids.is_empty() {
            return Err(PathError::NotAtom(a.clone()));
        }
    }

    let sorted = topo_sort(&g, atoms)?;

    let now = Utc::now();
    let id = generate_path_id(now);

    let p = PathFile {
        schema_version: 1,
        id: id.clone(),
        goal: goal.to_string(),
        created_at: now,
        target_atoms: sorted,
        cards: HashMap::new(),
    };

    save_path(&p)?;

    event_log::append(event_log::Event {
        ts: now,
        kind: "path_created".to_string(),
        path: id.clone(),
        atom: None,
        quiz: None,
    })?;

    Ok(id)
}

pub fn cmd_state(explicit_id: Option<&str>) -> Result<(), PathError> {
    let id = resolve_id(explicit_id)?;
    let p = load_path(&id)?;
    println!("path:        {}", p.id);
    println!("goal:        {}", p.goal);
    println!("created_at:  {}", p.created_at.to_rfc3339());
    println!("targets:     {}", p.target_atoms.len());
    for a in &p.target_atoms {
        println!("  - {a}");
    }
    println!("cards:       {}", p.cards.len());
    Ok(())
}

// ── Topological sort over the user-supplied target atoms ───────────

fn topo_sort(g: &Graph, atoms: &[String]) -> Result<Vec<String>, PathError> {
    let atom_set: HashSet<&str> = atoms.iter().map(|s| s.as_str()).collect();
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
    frontier.sort();

    let mut result = Vec::new();
    while !frontier.is_empty() {
        let node = frontier.remove(0);
        result.push(node.to_string());
        if let Some(neighbors) = adj.get(node) {
            let mut neighbors = neighbors.clone();
            neighbors.sort();
            for n in neighbors {
                let entry = indegree.get_mut(n).unwrap();
                *entry -= 1;
                if *entry == 0 {
                    frontier.push(n);
                    frontier.sort();
                }
            }
        }
    }

    if result.len() != atoms.len() {
        return Err(PathError::Cycle);
    }
    Ok(result)
}
