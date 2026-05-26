//! Learning-path data, per-path storage, and `mt new`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use libsql::{Connection, params};

use crate::event_log;
use crate::graph::Graph;
use crate::{Error, Result};

/// Immutable record of the learner's goal for a path. Written once at
/// `mt new`; never updated. All mutable per-path state lives in the
/// event log; FSRS state is the cards table maintained as a write-through
/// cache by `event_log::append`.
#[derive(Debug, Clone)]
pub struct PathFile {
    pub id: String,
    pub goal: String,
    pub created_at: DateTime<Utc>,
    pub target_atoms: Vec<String>,
}

// ── Storage layout ──────────────────────────────────────────────────

pub fn mt_home() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("MATHTUTOR_HOME") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var("HOME").map_err(|_| Error::NoHome)?;
    Ok(PathBuf::from(home).join(".mathtutor"))
}

pub fn paths_root() -> Result<PathBuf> {
    Ok(mt_home()?.join("paths"))
}

/// Per-path filesystem directory. Still used by the overlay layer
/// (`~/.mathtutor/paths/<id>/overlay.ayml`); event-log and path metadata
/// no longer live on disk.
pub fn path_dir(id: &str) -> Result<PathBuf> {
    Ok(paths_root()?.join(id))
}

pub fn generate_path_id(now: DateTime<Utc>) -> String {
    format!("p_{}", now.format("%Y_%m_%d_%H%M%S"))
}

// ── SQL helpers ─────────────────────────────────────────────────────

/// # Panics
/// Panics if `target_atoms.len()` doesn't fit in `i64` (≈9e18 targets).
pub async fn save_path(conn: &Connection, p: &PathFile) -> Result<()> {
    conn.execute(
        "INSERT INTO paths(id, goal, created_at) VALUES (?, ?, ?)",
        params![p.id.clone(), p.goal.clone(), p.created_at.to_rfc3339()],
    )
    .await?;
    for (i, atom) in p.target_atoms.iter().enumerate() {
        let position = i64::try_from(i).expect("position fits in i64");
        conn.execute(
            "INSERT INTO path_targets(path_id, atom_id, position) VALUES (?, ?, ?)",
            params![p.id.clone(), atom.clone(), position],
        )
        .await?;
    }
    Ok(())
}

pub async fn load_path(conn: &Connection, id: &str) -> Result<PathFile> {
    let mut rows = conn
        .query(
            "SELECT goal, created_at FROM paths WHERE id = ?",
            params![id.to_string()],
        )
        .await?;
    let row = rows.next().await?.ok_or(Error::NoPath)?;
    let goal: String = row.get(0)?;
    let created_str: String = row.get(1)?;
    let created_at = parse_ts(&created_str)?;

    let mut rows = conn
        .query(
            "SELECT atom_id FROM path_targets WHERE path_id = ? ORDER BY position ASC",
            params![id.to_string()],
        )
        .await?;
    let mut targets = Vec::new();
    while let Some(r) = rows.next().await? {
        targets.push(r.get::<String>(0)?);
    }

    Ok(PathFile {
        id: id.to_string(),
        goal,
        created_at,
        target_atoms: targets,
    })
}

/// "Most recent" = the path mentioned by the latest event, falling back
/// to the newest row in `paths` if there are no events yet. Mirrors the
/// old filesystem mtime semantic: a path the user is actively working in
/// stays sticky regardless of creation order.
pub async fn most_recent_id(conn: &Connection) -> Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT path_id FROM events ORDER BY id DESC LIMIT 1",
            params![],
        )
        .await?;
    if let Some(r) = rows.next().await? {
        return Ok(Some(r.get(0)?));
    }
    let mut rows = conn
        .query(
            "SELECT id FROM paths ORDER BY created_at DESC LIMIT 1",
            params![],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(r) => Some(r.get(0)?),
        None => None,
    })
}

pub async fn resolve_id(conn: &Connection, explicit: Option<&str>) -> Result<String> {
    if let Some(id) = explicit {
        return Ok(id.to_string());
    }
    most_recent_id(conn).await?.ok_or(Error::NoPath)
}

fn parse_ts(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| Error::BadTimestamp(format!("{s}: {e}")))
}

// ── Commands ────────────────────────────────────────────────────────

pub async fn cmd_new(
    conn: &Connection,
    goal: &str,
    ids: &[String],
    graph_dir: Option<&Path>,
) -> Result<String> {
    let g = Graph::load_default(graph_dir)?;
    let expanded = expand_to_atoms(&g, ids)?;
    let sorted = topo_sort(&g, &expanded)?;

    let now = Utc::now();
    let id = generate_path_id(now);

    let p = PathFile {
        id: id.clone(),
        goal: goal.to_string(),
        created_at: now,
        target_atoms: sorted,
    };

    let tx = conn.transaction().await?;
    save_path(&tx, &p).await?;
    event_log::append(&tx, &event_log::path_created(id.clone())).await?;
    tx.commit().await?;

    Ok(id)
}

/// Expand each input ID into a deduplicated set of atom IDs.
///
/// - An atom (leaf node) is included as-is.
/// - A cluster (non-leaf node) is expanded to all atomic descendants.
/// - A bare area prefix (e.g. `tx`) — not itself a node, since the
///   graph's roots are topic-level (e.g. `tx.1`) — is expanded to all
///   atoms whose ID starts with `<prefix>.`.
fn expand_to_atoms(g: &Graph, ids: &[String]) -> Result<Vec<String>> {
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
                    return Err(Error::EmptyCluster(id.clone()));
                }
            }
            None if !id.contains('.') => {
                collect_atoms_by_prefix(g, id, &mut seen, &mut out);
                if out.len() == before {
                    return Err(Error::UnknownId(id.clone()));
                }
            }
            None => return Err(Error::UnknownId(id.clone())),
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

fn topo_sort(g: &Graph, atoms: &[String]) -> Result<Vec<String>> {
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
        return Err(Error::Cycle);
    }
    Ok(result)
}
