//! Learning path records and path management commands.

use std::collections::{BinaryHeap, HashMap, HashSet};

use chrono::{DateTime, Utc};

use crate::db::Db;
use crate::graph::{Graph, natural_id_cmp};
use crate::progress::TargetProgress;
use crate::types::Strategy;
use crate::view::Want;
use crate::{Error, Result};

pub use crate::schema::PathFile;

impl PathFile {
    /// Return a new path toward `targets`, or fail if `targets` is empty or `g` cannot expand a target.
    ///
    /// The path stores each cluster target as given, without its atoms.
    pub fn create(g: &Graph, goal: &str, targets: &[String], strategy: Strategy) -> Result<Self> {
        if targets.is_empty() {
            return Err(Error::NoTargets);
        }
        g.expand_to_atoms(targets)?;
        let now = Utc::now();
        Ok(Self {
            id: generate_path_id(now),
            goal: goal.to_string(),
            created_at: now,
            targets: dedup_preserving_order(targets),
            strategy,
            subpath: Vec::new(),
        })
    }

    /// Expand stored targets into a topologically sorted list of atom IDs.
    pub fn resolve_targets(&self, g: &Graph) -> Result<Vec<String>> {
        let atoms = g.expand_to_atoms(&self.targets)?;
        topo_sort(g, &atoms)
    }

    /// Expand each stored target that `g` can expand, and keep the IDs of the other targets.
    pub fn resolve_each_target(&self, g: &Graph) -> Result<ResolvedTargets> {
        let mut atoms: Vec<String> = Vec::new();
        let mut unresolved = Vec::new();
        for target in &self.targets {
            match g.expand_to_atoms(std::slice::from_ref(target)) {
                Ok(expanded) => atoms.extend(expanded),
                Err(_) => unresolved.push(target.clone()),
            }
        }
        let atoms = dedup_preserving_order(&atoms);
        Ok(ResolvedTargets {
            atoms: topo_sort(g, &atoms)?,
            unresolved,
        })
    }
}

/// Target atoms of a path, with the stored target IDs that the curriculum cannot expand.
pub struct ResolvedTargets {
    /// Topologically sorted atoms of the targets that the curriculum can expand.
    pub atoms: Vec<String>,
    /// Stored target IDs that the curriculum does not contain or that have zero atoms.
    pub unresolved: Vec<String>,
}

/// Generate a unique path ID from `now` and a random 4-character suffix.
pub fn generate_path_id(now: DateTime<Utc>) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let suffix: String = (0..4)
        .map(|_| char::from(ALPHABET[rand::random_range(0..ALPHABET.len())]))
        .collect();
    format!("p_{}_{suffix}", now.format("%Y_%m_%d_%H%M%S"))
}

/// Create a new learning path toward `targets` and return its ID.
pub async fn cmd_path_new(
    db: &Db,
    goal: &str,
    targets: &[String],
    strategy: Strategy,
) -> Result<String> {
    let p = PathFile::create(&*db.curriculum().graph()?, goal, targets, strategy)?;
    store_new_path(db, &p).await?;
    Ok(p.id)
}

/// Store the new path `p` and its creation event.
pub async fn store_new_path(db: &Db, p: &PathFile) -> Result<()> {
    db.transact(&Want::nothing(), |view, changes| {
        changes.create_path(view, p)
    })
    .await
}

/// Update the traversal strategy for a learning path and return its ID.
pub async fn cmd_path_strategy(
    db: &Db,
    explicit_id: Option<&str>,
    strategy: Strategy,
) -> Result<String> {
    let want = Want::path(explicit_id).without_overlay();
    db.transact(&want, |view, changes| {
        let p = PathFile {
            strategy,
            ..view.path()?.file.clone()
        };
        changes.replace_path(&p);
        Ok(p.id)
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
    /// Time of the latest write to the path.
    pub updated_at: chrono::DateTime<Utc>,
    /// Progress over the targets that the curriculum of this release can expand.
    pub targets: TargetProgress,
    /// Stored target IDs that the curriculum of this release cannot expand, such as a renamed atom.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unresolved_targets: Vec<String>,
}

/// Learning paths of the user, ordered by creation time.
#[derive(Debug, serde::Serialize)]
pub struct PathListView {
    pub paths: Vec<PathSummary>,
}

/// Print all learning path summaries as AYML (`mt path list`).
pub async fn cmd_path_list(db: &Db) -> Result<()> {
    crate::print_ayml(&compute_path_list(db).await?)
}

/// Compute the progress summary of every learning path.
///
/// A path with targets that the curriculum cannot expand reports those target IDs instead of failing the list.
pub async fn compute_path_list(db: &Db) -> Result<PathListView> {
    let view = db.snapshot(&Want::all_paths()).await?;
    let g = view.graph()?;
    let paths = view
        .paths()
        .into_iter()
        .map(|p| {
            let resolved = p.file.resolve_each_target(&g)?;
            Ok(PathSummary {
                id: p.file.id.clone(),
                goal: p.file.goal.clone(),
                strategy: p.file.strategy,
                created_at: p.file.created_at,
                updated_at: p.updated_at,
                targets: TargetProgress::of(&g, &resolved.atoms, &p.progress()),
                unresolved_targets: resolved.unresolved,
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
        for p in g.atomic_prerequisites(atom) {
            if let Some(&p) = atom_set.get(p.as_str()) {
                adj.entry(p).or_default().push(atom.as_str());
                *indegree.get_mut(atom.as_str()).unwrap() += 1;
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
    use super::{generate_path_id, topo_sort};
    use std::collections::HashSet;

    #[test]
    fn path_ids_share_the_time_and_differ_in_a_random_suffix() {
        let now = chrono::Utc::now();
        let prefix = format!("p_{}_", now.format("%Y_%m_%d_%H%M%S"));
        let ids: HashSet<String> = (0..20).map(|_| generate_path_id(now)).collect();
        assert!(ids.len() > 1, "{ids:?}");
        for id in &ids {
            let suffix = id.strip_prefix(&prefix).expect("time prefix");
            assert_eq!(suffix.len(), 4);
            assert!(
                suffix
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            );
        }
    }
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
