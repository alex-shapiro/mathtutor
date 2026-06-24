//! Learning-path data, per-path storage, and `mt path new`/`mt path list`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use libsql::{Connection, params};

use crate::db;
use crate::event_log;
use crate::graph::Graph;
use crate::types::Strategy;
use crate::{Error, Result};

/// Per-path record: the learner's goal and targets (fixed at `mt path
/// new`) plus the mutable navigation `strategy`. Learning history lives in
/// the event log; the top-down subpath lives in the `path_subpath` table.
///
/// `targets` holds the IDs the learner chose verbatim — atoms, clusters,
/// or area roots. They are stored as given and expanded to atoms against
/// the live graph by [`PathFile::resolve_targets`], so the resolved set
/// tracks curriculum edits (e.g. an atom later split into a cluster).
#[derive(Debug, Clone)]
pub struct PathFile {
    pub id: String,
    pub goal: String,
    pub created_at: DateTime<Utc>,
    pub targets: Vec<String>,
    pub strategy: Strategy,
}

impl PathFile {
    /// Expand the stored targets to a topo-sorted set of atom IDs against
    /// `g`. Resolution runs on every load so cluster/area targets always
    /// reflect the current curriculum. Errors if a target no longer
    /// resolves to any atom (unknown ID or empty cluster).
    pub fn resolve_targets(&self, g: &Graph) -> Result<Vec<String>> {
        let atoms = g.expand_to_atoms(&self.targets)?;
        topo_sort(g, &atoms)
    }
}

// ── Storage layout ──────────────────────────────────────────────────

pub fn mt_home() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("MATHTUTOR_HOME") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var("HOME").map_err(|_| Error::NoHome)?;
    Ok(PathBuf::from(home).join(".mathtutor"))
}

pub fn generate_path_id(now: DateTime<Utc>) -> String {
    format!("p_{}", now.format("%Y_%m_%d_%H%M%S"))
}

// ── SQL helpers ─────────────────────────────────────────────────────

/// # Panics
/// Panics if `targets.len()` doesn't fit in `i64` (≈9e18 targets).
pub async fn save_path(conn: &Connection, p: &PathFile) -> Result<()> {
    conn.execute(
        "INSERT INTO paths(id, goal, created_at, strategy) VALUES (?, ?, ?, ?)",
        params![
            p.id.as_str(),
            p.goal.as_str(),
            db::format_ts(p.created_at),
            p.strategy.as_str(),
        ],
    )
    .await?;
    for (i, target) in p.targets.iter().enumerate() {
        let position = i64::try_from(i).expect("position fits in i64");
        conn.execute(
            "INSERT INTO path_targets(path_id, target_id, position) VALUES (?, ?, ?)",
            params![p.id.as_str(), target.as_str(), position],
        )
        .await?;
    }
    Ok(())
}

pub async fn load_path(conn: &Connection, id: &str) -> Result<PathFile> {
    let mut rows = conn
        .query(
            "SELECT goal, created_at, strategy FROM paths WHERE id = ?",
            params![id],
        )
        .await?;
    let row = rows.next().await?.ok_or(Error::NoPath)?;
    let goal: String = row.get(0)?;
    let created_str: String = row.get(1)?;
    let created_at = db::parse_ts(&created_str)?;
    let strategy: Strategy = row.get::<String>(2)?.parse()?;

    let mut rows = conn
        .query(
            "SELECT target_id FROM path_targets WHERE path_id = ? ORDER BY position ASC",
            params![id],
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
        targets,
        strategy,
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

// ── Commands ────────────────────────────────────────────────────────

pub async fn cmd_path_new(
    conn: &Connection,
    goal: &str,
    ids: &[String],
    strategy: Strategy,
    graph_dir: Option<&Path>,
) -> Result<String> {
    let g = Graph::load_default(graph_dir)?;
    // Validate each ID resolves to at least one atom, but store the IDs
    // verbatim — clusters and area roots are kept as-is and re-expanded on
    // load so the target set tracks later curriculum edits.
    g.expand_to_atoms(ids)?;
    let targets = dedup_preserving_order(ids);

    let now = Utc::now();
    let id = generate_path_id(now);

    let p = PathFile {
        id: id.clone(),
        goal: goal.to_string(),
        created_at: now,
        targets,
        strategy,
    };

    let tx = conn.transaction().await?;
    save_path(&tx, &p).await?;
    event_log::append(&tx, &event_log::path_created(id.clone())).await?;
    tx.commit().await?;

    Ok(id)
}

/// Switch a path's traversal strategy. Mutates the `paths.strategy`
/// column directly — strategy is navigation config, not learning history,
/// so no event is logged. Switching to bottom-up leaves any stored
/// subpath inert (the DFS ignores it) rather than clearing it.
pub async fn cmd_path_strategy(
    conn: &Connection,
    explicit_id: Option<&str>,
    strategy: Strategy,
) -> Result<String> {
    let id = resolve_id(conn, explicit_id).await?;
    let changed = conn
        .execute(
            "UPDATE paths SET strategy = ? WHERE id = ?",
            params![strategy.as_str(), id.as_str()],
        )
        .await?;
    if changed == 0 {
        return Err(Error::NoPath);
    }
    Ok(id)
}

/// Compact summary of one path. Same fields as `mt path state` shows
/// for a single path, so callers can format both with the same logic.
#[derive(Debug, serde::Serialize)]
pub struct PathSummary {
    pub id: String,
    pub goal: String,
    pub strategy: Strategy,
    pub created_at: chrono::DateTime<Utc>,
    pub targets: usize,
    pub learned: usize,
    pub learned_pct: usize,
}

#[derive(serde::Serialize)]
struct PathListView {
    paths: Vec<PathSummary>,
}

/// List every path with goal, creation time, target count, and progress
/// percent. AYML on stdout — same envelope conventions as `mt path next`.
pub async fn cmd_path_list(conn: &Connection, graph_dir: Option<&Path>) -> Result<()> {
    let summaries = list_summaries(conn, graph_dir).await?;
    let view = PathListView { paths: summaries };
    let text = ayml::to_string(&view).map_err(|e| Error::AymlSerialize(e.to_string()))?;
    print!("{text}");
    Ok(())
}

async fn list_summaries(conn: &Connection, graph_dir: Option<&Path>) -> Result<Vec<PathSummary>> {
    let g = Graph::load_for_path(conn, graph_dir).await?;
    let mut rows = conn
        .query(
            "SELECT id, goal, created_at FROM paths ORDER BY created_at ASC",
            params![],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        let id: String = row.get(0)?;
        let goal: String = row.get(1)?;
        let created_str: String = row.get(2)?;
        let created_at = db::parse_ts(&created_str)?;
        let p = load_path(conn, &id).await?;
        let progress = crate::progress::PathProgress::load(conn, &id).await?;
        let atoms = p.resolve_targets(&g)?;
        let targets = atoms.len();
        let learned = atoms
            .iter()
            .filter(|a| crate::scheduler::is_atom_complete(&g, &progress, a))
            .count();
        let learned_pct = (learned * 100).checked_div(targets).unwrap_or(0);
        out.push(PathSummary {
            id,
            goal,
            strategy: p.strategy,
            created_at,
            targets,
            learned,
            learned_pct,
        });
    }
    Ok(out)
}

/// Deduplicate `ids` while preserving first-seen order.
fn dedup_preserving_order(ids: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    ids.iter()
        .filter(|id| seen.insert((*id).clone()))
        .cloned()
        .collect()
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
