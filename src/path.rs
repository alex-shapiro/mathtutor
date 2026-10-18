//! Learning-path data, per-path storage, and `mt path new`/`mt path list`.

use std::collections::{BinaryHeap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::{Value as Json, json};

use crate::db::{self, Db};
use crate::event_log;
use crate::graph::{Graph, natural_id_cmp};
use crate::types::Strategy;
use crate::view::{Changes, PathSel, View, Want};
use crate::{Error, Result};

/// Per-path record: the learner's goal and targets (fixed at `mt path
/// new`), the mutable navigation `strategy`, and the top-down subpath.
/// Learning history lives in the path's event log.
///
/// `targets` holds the IDs the learner chose verbatim (atoms or clusters).
/// They are stored as given and expanded to atoms against the live graph
/// by [`PathFile::resolve_targets`], so the resolved set tracks curriculum
/// edits (e.g. an atom later split into a cluster). `subpath` is kept the
/// same way.
#[derive(Debug, Clone)]
pub struct PathFile {
    pub id: String,
    pub goal: String,
    pub created_at: DateTime<Utc>,
    /// Time of the path's latest event. Stored apart from the path row,
    /// so logging events never writes the row.
    pub updated_at: DateTime<Utc>,
    pub targets: Vec<String>,
    pub strategy: Strategy,
    pub subpath: Vec<String>,
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

/// A path id from its creation time plus a random suffix, so paths created
/// within the same second get distinct ids.
pub fn generate_path_id(now: DateTime<Utc>) -> String {
    use std::hash::{BuildHasher, RandomState};
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut n = RandomState::new().hash_one(now);
    let suffix: String = (0..4)
        .map(|_| {
            let c = ALPHABET[(n % 36) as usize] as char;
            n /= 36;
            c
        })
        .collect();
    format!("p_{}_{suffix}", now.format("%Y_%m_%d_%H%M%S"))
}

// ── Rows ────────────────────────────────────────────────────────────

/// The path a `paths` row holds.
pub fn decode(r: &Json) -> Result<PathFile> {
    let s = |k: &str| {
        r[k].as_str()
            .map(str::to_string)
            .ok_or_else(|| Error::BadRow(format!("paths.{k}: {r}")))
    };
    let ids = |k: &str| -> Result<Vec<String>> {
        r[k].as_array()
            .ok_or_else(|| Error::BadRow(format!("paths.{k}: {r}")))?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| Error::BadRow(format!("paths.{k}: {r}")))
            })
            .collect()
    };
    Ok(PathFile {
        id: s("id")?,
        goal: s("goal")?,
        created_at: db::parse_ts(&s("created_at")?)?,
        updated_at: db::parse_ts(&s("created_at")?)?,
        targets: ids("targets")?,
        strategy: s("strategy")?.parse()?,
        subpath: ids("subpath")?,
    })
}

/// Insert `p` and log its `path_created` event.
pub fn insert(view: &View, changes: &mut Changes, p: &PathFile) -> Result<()> {
    changes.op(json!({ "op": "insert_row", "table": "paths", "row": {
        "id": p.id, "goal": p.goal, "created_at": db::format_ts(p.created_at),
        "strategy": p.strategy.as_str(),
        "targets": p.targets, "subpath": p.subpath,
    }}));
    let mut created = event_log::path_created(p.id.clone());
    created.ts = p.created_at;
    changes.append(view, &created)
}

/// Replace the stored subpath of `path_id`.
pub fn set_subpath(changes: &mut Changes, path_id: &str, atoms: &[String]) {
    changes.op(
        json!({ "op": "update_row", "table": "paths", "pk": [path_id],
        "set": { "subpath": atoms } }),
    );
}

// ── Commands ────────────────────────────────────────────────────────

pub async fn cmd_path_new(
    db: &Db,
    goal: &str,
    ids: &[String],
    strategy: Strategy,
    graph_dir: Option<&Path>,
) -> Result<String> {
    let g = Graph::load_default(graph_dir)?;
    // Validate each ID resolves to at least one atom, but store the IDs
    // verbatim — clusters are kept as-is and re-expanded on load so the
    // target set tracks later curriculum edits.
    g.expand_to_atoms(ids)?;
    let targets = dedup_preserving_order(ids);

    let now = Utc::now();
    let p = PathFile {
        id: generate_path_id(now),
        goal: goal.to_string(),
        created_at: now,
        updated_at: now,
        targets,
        strategy,
        subpath: Vec::new(),
    };
    let want = Want {
        path: PathSel::None,
        overlay: false,
        all_paths: false,
    };
    db.transact(&want, |view, changes| insert(view, changes, &p))
        .await?;
    Ok(p.id)
}

/// Switch a path's traversal strategy. Strategy is navigation config, not
/// learning history, so no event is logged. Switching to bottom-up leaves
/// any stored subpath inert (the DFS ignores it) rather than clearing it.
pub async fn cmd_path_strategy(
    db: &Db,
    explicit_id: Option<&str>,
    strategy: Strategy,
) -> Result<String> {
    let want = Want {
        path: PathSel::from(explicit_id),
        overlay: false,
        all_paths: false,
    };
    db.transact(&want, |view, changes| {
        let id = view.path()?.file.id.clone();
        changes.op(json!({ "op": "update_row", "table": "paths", "pk": [id],
            "set": { "strategy": strategy.as_str() } }));
        Ok(id)
    })
    .await
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
pub async fn cmd_path_list(db: &Db, graph_dir: Option<&Path>) -> Result<()> {
    let summaries = list_summaries(db, graph_dir).await?;
    let view = PathListView { paths: summaries };
    let text = ayml::to_string(&view).map_err(|e| Error::AymlSerialize(e.to_string()))?;
    print!("{text}");
    Ok(())
}

/// Summaries of every path, oldest first.
pub async fn list_summaries(db: &Db, graph_dir: Option<&Path>) -> Result<Vec<PathSummary>> {
    let want = Want {
        path: PathSel::None,
        overlay: true,
        all_paths: true,
    };
    let view = db.snapshot(&want).await?;
    let g = view.graph(graph_dir)?;
    view.paths
        .iter()
        .map(|p| {
            let progress = p.progress();
            let atoms = p.file.resolve_targets(&g)?;
            let targets = atoms.len();
            let learned = atoms
                .iter()
                .filter(|a| crate::scheduler::is_atom_complete(&g, &progress, a))
                .count();
            Ok(PathSummary {
                id: p.file.id.clone(),
                goal: p.file.goal.clone(),
                strategy: p.file.strategy,
                created_at: p.file.created_at,
                targets,
                learned,
                learned_pct: (learned * 100).checked_div(targets).unwrap_or(0),
            })
        })
        .collect()
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

    // Kahn's algorithm with a min-heap frontier so topo ties break by
    // natural id order: siblings come out `nn.1.1.1, nn.1.1.2, …` rather
    // than `…1, …10, …2`. The heap keeps this O(n log n) with no per-
    // comparison allocation (see `natural_id_cmp`).
    let mut frontier: BinaryHeap<NaturalOrd> = indegree
        .iter()
        .filter_map(|(k, &v)| if v == 0 { Some(NaturalOrd(k)) } else { None })
        .collect();

    let mut result = Vec::new();
    while let Some(NaturalOrd(node)) = frontier.pop() {
        result.push(node.to_string());
        if let Some(neighbors) = adj.get(node) {
            for n in neighbors {
                let entry = indegree.get_mut(n).unwrap();
                *entry -= 1;
                if *entry == 0 {
                    frontier.push(NaturalOrd(n));
                }
            }
        }
    }

    if result.len() != atoms.len() {
        return Err(Error::Cycle);
    }
    Ok(result)
}

/// `&str` id wrapped to pop in natural order from a `BinaryHeap` (a
/// max-heap), so `Ord` is reversed: the natural-smallest id is "greatest".
struct NaturalOrd<'a>(&'a str);

impl Ord for NaturalOrd<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        natural_id_cmp(other.0, self.0)
    }
}
impl PartialOrd for NaturalOrd<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for NaturalOrd<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl Eq for NaturalOrd<'_> {}

#[cfg(test)]
mod tests {
    use super::topo_sort;
    use crate::graph::{FlatConcept, Graph};
    use std::collections::HashMap;

    /// Build a graph from `(id, prerequisites)` pairs; other fields unused
    /// by `topo_sort`.
    fn graph(nodes: &[(&str, &[&str])]) -> Graph {
        let by_id = nodes
            .iter()
            .map(|(id, prereqs)| {
                let c = FlatConcept {
                    id: (*id).to_string(),
                    name: String::new(),
                    description: None,
                    prerequisites: prereqs.iter().map(|p| (*p).to_string()).collect(),
                    children_ids: Vec::new(),
                    lesson: None,
                    quizzes: Vec::new(),
                };
                ((*id).to_string(), c)
            })
            .collect::<HashMap<_, _>>();
        Graph { by_id }
    }

    #[test]
    fn topo_sort_breaks_ties_in_natural_id_order() {
        // The bug: lexicographic tie-breaking jumped `nn.1.1.1` straight to
        // `nn.1.10.1`, skipping `nn.1.1.2`. With a prereq chain only along
        // the `.1.1.x` spine, the independent `.1.10.1` must still sort
        // after the lower-numbered siblings, not between `…1.1` and `…1.2`.
        let g = graph(&[
            ("nn.1.1.1", &[]),
            ("nn.1.1.2", &["nn.1.1.1"]),
            ("nn.1.2.1", &["nn.1.1.1"]),
            ("nn.1.10.1", &["nn.1.1.1"]),
        ]);
        let atoms: Vec<String> = ["nn.1.1.1", "nn.1.1.2", "nn.1.2.1", "nn.1.10.1"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let order = topo_sort(&g, &atoms).unwrap();
        assert_eq!(order, vec!["nn.1.1.1", "nn.1.1.2", "nn.1.2.1", "nn.1.10.1"]);
    }

    #[test]
    fn topo_sort_respects_prerequisites_over_id_order() {
        // A lower-numbered atom that depends on a higher-numbered one must
        // still come after it, regardless of natural id order.
        let g = graph(&[("nn.1.1.1", &["nn.1.9.1"]), ("nn.1.9.1", &[])]);
        let atoms = vec!["nn.1.1.1".to_string(), "nn.1.9.1".to_string()];
        let order = topo_sort(&g, &atoms).unwrap();
        assert_eq!(order, vec!["nn.1.9.1", "nn.1.1.1"]);
    }
}
