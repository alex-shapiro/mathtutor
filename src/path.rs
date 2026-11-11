//! Learning path records and path management commands.

use std::collections::{BinaryHeap, HashMap, HashSet};
use std::path::PathBuf;

use chrono::{DateTime, Utc};

use crate::db::Db;
use crate::graph::{Graph, natural_id_cmp};
use crate::progress::{PathCompletion, TargetProgress};
use crate::types::Strategy;
use crate::view::Want;
use crate::{Error, Result};

pub use crate::schema::PathFile;

impl PathFile {
    /// Expand stored targets into a topologically sorted list of atom IDs.
    pub fn resolve_targets(&self, g: &Graph) -> Result<Vec<String>> {
        let atoms = g.expand_to_atoms(&self.targets)?;
        topo_sort(g, &atoms)
    }
}

/// Resolve the local Math Tutor home directory from `MATHTUTOR_HOME` or `$HOME/.mathtutor`.
pub fn mt_home() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("MATHTUTOR_HOME") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var("HOME").map_err(|_| Error::NoHome)?;
    Ok(PathBuf::from(home).join(".mathtutor"))
}

/// Generate a unique path ID from `now` and a random 4-character suffix.
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

/// Create a new learning path and return its ID.
pub async fn cmd_path_new(
    db: &Db,
    goal: &str,
    ids: &[String],
    strategy: Strategy,
) -> Result<String> {
    let g = db.curriculum().graph()?;
    // Validate that each target expands to at least 1 atom while storing cluster IDs unexpanded.
    g.expand_to_atoms(ids)?;
    let targets = dedup_preserving_order(ids);

    let now = Utc::now();
    let p = PathFile {
        id: generate_path_id(now),
        goal: goal.to_string(),
        created_at: now,
        targets,
        strategy,
        subpath: Vec::new(),
    };
    db.transact(&Want::nothing(), |view, changes| {
        changes.create_path(view, &p)
    })
    .await?;
    Ok(p.id)
}

/// Update the traversal strategy for a learning path and return its ID.
pub async fn cmd_path_strategy(
    db: &Db,
    explicit_id: Option<&str>,
    strategy: Strategy,
) -> Result<String> {
    let want = Want::path(explicit_id).without_overlay();
    db.transact(&want, |view, changes| {
        let id = view.path()?.file.id.clone();
        changes.set_strategy(&id, strategy);
        Ok(id)
    })
    .await
}

/// Goal and target progress of a learning path.
#[derive(Debug, serde::Serialize)]
pub struct PathSummary {
    pub id: String,
    pub goal: String,
    pub strategy: Strategy,
    pub created_at: chrono::DateTime<Utc>,
    pub targets: TargetProgress,
}

/// Learning paths of the user, ordered by creation time.
#[derive(Debug, serde::Serialize)]
pub struct PathListView {
    pub paths: Vec<PathSummary>,
}

/// Print all learning path summaries as AYML (`mt path list`).
pub async fn cmd_path_list(db: &Db) -> Result<()> {
    let view = compute_path_list(db).await?;
    let text = ayml::to_string(&view).map_err(|e| Error::AymlSerialize(e.to_string()))?;
    print!("{text}");
    Ok(())
}

/// Compute the progress summary of every learning path.
pub async fn compute_path_list(db: &Db) -> Result<PathListView> {
    let view = db.snapshot(&Want::all_paths()).await?;
    let g = view.graph()?;
    let paths = view
        .paths()
        .into_iter()
        .map(|p| {
            let atoms = p.file.resolve_targets(&g)?;
            Ok(PathSummary {
                id: p.file.id.clone(),
                goal: p.file.goal.clone(),
                strategy: p.file.strategy,
                created_at: p.file.created_at,
                targets: PathCompletion::of(&g, &atoms, &p.progress()).targets,
            })
        })
        .collect::<Result<_>>()?;
    Ok(PathListView { paths })
}

fn dedup_preserving_order(ids: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    ids.iter()
        .filter(|id| seen.insert((*id).clone()))
        .cloned()
        .collect()
}

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

    // Use a min-heap ordered by `natural_id_cmp` to break topological ties deterministically.
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

// Reverse `natural_id_cmp` so `BinaryHeap` pops the smallest ID first.
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
        Graph::of_concepts(by_id.into_values())
    }

    #[test]
    fn topo_sort_breaks_ties_in_natural_id_order() {
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
        let g = graph(&[("nn.1.1.1", &["nn.1.9.1"]), ("nn.1.9.1", &[])]);
        let atoms = vec!["nn.1.1.1".to_string(), "nn.1.9.1".to_string()];
        let order = topo_sort(&g, &atoms).unwrap();
        assert_eq!(order, vec!["nn.1.9.1", "nn.1.1.1"]);
    }
}
