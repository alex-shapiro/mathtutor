//! Curriculum graph: AYML-backed types, loader, and `mt graph check`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use include_dir::{Dir, include_dir};
use libsql::Connection;
use serde::{Deserialize, Serialize};

use crate::types::{Difficulty, QuizType};
use crate::{Error, Result};

/// Curriculum bytes baked into the binary at compile time.
/// The `--graph DIR` CLI flag and `MT_GRAPH` env override this
/// for development against a working tree.
static EMBEDDED_GRAPH: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/curriculum/graph");

// ── Manifest ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub areas: Vec<ManifestArea>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ManifestArea {
    pub prefix: String,
    pub slug: String,
    pub file: String,
    pub summary: String,
}

// ── Raw area-file shape (handles both v1 and v2) ────────────────────
//
// These types round-trip: deserialize on read, re-serialize on write.
// Field order matches the on-disk convention; empty / `None` fields
// are skip-serialized to keep files free of default-valued lines.

#[derive(Debug, Deserialize, Serialize)]
struct AreaFileRaw {
    schema_version: u32,
    area: String,
    prefix: String,
    summary: String,
    motivation: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    cross_references: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    topics: Option<Vec<TopicRaw>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    children: Option<Vec<NodeRaw>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct TopicRaw {
    id: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    why: Option<String>,
    leaves: Vec<LeafRaw>,
}

#[derive(Debug, Deserialize, Serialize)]
struct LeafRaw {
    id: String,
    name: String,
    description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    prerequisites: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lesson: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    quizzes: Option<Vec<Quiz>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    relevant_for: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    difficulty: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    terminal: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct NodeRaw {
    id: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    prerequisites: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    children: Option<Vec<NodeRaw>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lesson: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    quizzes: Option<Vec<Quiz>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    relevant_for: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    difficulty: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    terminal: bool,
}

// ── Unified concept tree ────────────────────────────────────────────

/// Concept node
#[derive(Debug, Clone)]
struct Concept {
    id: String,
    name: String,
    description: Option<String>,
    prerequisites: Vec<String>,
    children: Vec<Concept>,
    lesson: Option<String>,
    quizzes: Vec<Quiz>,
    /// Terminal topic with no dependents
    terminal: bool,
}

/// A single quiz card. Serializable for AYML round-trip (shipped
/// curriculum, `mt graph dump`) and as the in-memory shape used by
/// the scheduler and merge logic.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Quiz {
    pub id: String,
    pub difficulty: Difficulty,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<QuizType>,
    pub question: String,
    pub answer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rubric: Option<String>,
}

impl Concept {
    fn is_atom(&self) -> bool {
        self.children.is_empty()
    }
}

impl AreaFileRaw {
    fn into_concepts(self) -> Vec<Concept> {
        match self.schema_version {
            1 => self
                .topics
                .unwrap_or_default()
                .into_iter()
                .map(|t| Concept {
                    id: t.id,
                    name: t.name,
                    description: t.why,
                    prerequisites: Vec::new(),
                    children: t
                        .leaves
                        .into_iter()
                        .map(|l| Concept {
                            id: l.id,
                            name: l.name,
                            description: Some(l.description),
                            prerequisites: l.prerequisites,
                            children: Vec::new(),
                            lesson: l.lesson,
                            quizzes: l.quizzes.unwrap_or_default(),
                            terminal: l.terminal,
                        })
                        .collect(),
                    lesson: None,
                    quizzes: Vec::new(),
                    terminal: false,
                })
                .collect(),
            2 => self
                .children
                .unwrap_or_default()
                .into_iter()
                .map(node_to_concept)
                .collect(),
            _ => Vec::new(),
        }
    }
}

fn node_to_concept(n: NodeRaw) -> Concept {
    Concept {
        id: n.id,
        name: n.name,
        description: n.description,
        prerequisites: n.prerequisites,
        children: n
            .children
            .unwrap_or_default()
            .into_iter()
            .map(node_to_concept)
            .collect(),
        lesson: n.lesson,
        quizzes: n.quizzes.unwrap_or_default(),
        terminal: n.terminal,
    }
}

/// Synthetic area-root node (keyed by the area prefix) wrapping the area's
/// top-level concepts, so an area resolves and is looked up like any other
/// cluster. Identity comes from the manifest entry, children from the file.
fn area_root_concept(entry: &ManifestArea, children: Vec<Concept>) -> Concept {
    Concept {
        id: entry.prefix.clone(),
        name: entry.slug.clone(),
        description: Some(entry.summary.clone()),
        prerequisites: Vec::new(),
        children,
        lesson: None,
        quizzes: Vec::new(),
        terminal: false,
    }
}

// ── Loaders ─────────────────────────────────────────────────────────

fn load_manifest(path: &Path) -> Result<Manifest> {
    let reader = open_reader(path)?;
    ayml::from_reader(reader).map_err(|e| Error::AymlParse {
        path: path.to_path_buf(),
        message: e.to_string(),
    })
}

fn load_area(path: &Path) -> Result<AreaFileRaw> {
    let reader = open_reader(path)?;
    ayml::from_reader(reader).map_err(|e| Error::AymlParse {
        path: path.to_path_buf(),
        message: e.to_string(),
    })
}

fn open_reader(path: &Path) -> Result<BufReader<File>> {
    let file = File::open(path).map_err(|e| Error::FileIo {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(BufReader::new(file))
}

// ── Embedded loaders ────────────────────────────────────────────────

/// Display-only sentinel path used in `Error::AymlParse` when reading
/// the compiled-in curriculum. Lets the error pinpoint the offending
/// file without lying about its location on disk.
fn embedded_path(name: &str) -> PathBuf {
    PathBuf::from(format!("<embedded>/{name}"))
}

fn embedded_file_str(name: &str) -> Result<&'static str> {
    let file = EMBEDDED_GRAPH.get_file(name).ok_or_else(|| Error::FileIo {
        path: embedded_path(name),
        source: std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "file not found in embedded curriculum",
        ),
    })?;
    file.contents_utf8().ok_or_else(|| Error::AymlParse {
        path: embedded_path(name),
        message: "embedded file is not valid UTF-8".into(),
    })
}

fn load_manifest_embedded() -> Result<Manifest> {
    let s = embedded_file_str("manifest.ayml")?;
    ayml::from_str(s).map_err(|e| Error::AymlParse {
        path: embedded_path("manifest.ayml"),
        message: e.to_string(),
    })
}

fn load_area_embedded(filename: &str) -> Result<AreaFileRaw> {
    let s = embedded_file_str(filename)?;
    ayml::from_str(s).map_err(|e| Error::AymlParse {
        path: embedded_path(filename),
        message: e.to_string(),
    })
}

/// Resolve where to load curriculum from, honoring (in order) the
/// explicit `--graph DIR` flag, the `MT_GRAPH` env var, and otherwise
/// the embedded copy.
pub fn load_manifest_default(explicit: Option<&Path>) -> Result<Manifest> {
    if let Some(p) = explicit {
        return load_manifest(&p.join("manifest.ayml"));
    }
    if let Some(env_path) = std::env::var_os("MT_GRAPH") {
        return load_manifest(&Path::new(&env_path).join("manifest.ayml"));
    }
    load_manifest_embedded()
}

fn load_area_default(explicit: Option<&Path>, filename: &str) -> Result<AreaFileRaw> {
    if let Some(p) = explicit {
        return load_area(&p.join(filename));
    }
    if let Some(env_path) = std::env::var_os("MT_GRAPH") {
        return load_area(&Path::new(&env_path).join(filename));
    }
    load_area_embedded(filename)
}

// ── Flat graph (for scheduler / `mt path new`) ─────────────────────

/// Flattened lookup view: every concept (cluster + atom) keyed by ID.
/// Built once via `Graph::load`.
#[derive(Debug)]
pub struct Graph {
    pub by_id: HashMap<String, FlatConcept>,
}

#[derive(Debug, Clone)]
pub struct FlatConcept {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub prerequisites: Vec<String>,
    pub children_ids: Vec<String>,
    pub lesson: Option<String>,
    pub quizzes: Vec<Quiz>,
}

impl Graph {
    pub fn load(graph_dir: &Path) -> Result<Self> {
        let manifest = load_manifest(&graph_dir.join("manifest.ayml"))?;
        let mut by_id = HashMap::new();
        for entry in &manifest.areas {
            let raw = load_area(&graph_dir.join(&entry.file))?;
            flatten(area_root_concept(entry, raw.into_concepts()), &mut by_id);
        }
        Ok(Self { by_id })
    }

    /// Load curriculum from compiled-in bytes
    pub fn load_embedded() -> Result<Self> {
        let manifest = load_manifest_embedded()?;
        let mut by_id = HashMap::new();
        for entry in &manifest.areas {
            let raw = load_area_embedded(&entry.file)?;
            flatten(area_root_concept(entry, raw.into_concepts()), &mut by_id);
        }
        Ok(Self { by_id })
    }

    /// Source priority: explicit `--graph DIR` → `MT_GRAPH` env →
    /// embedded copy. The embedded variant is the default for shipped
    /// binaries; the others exist for development against a working
    /// tree.
    pub fn load_default(explicit: Option<&Path>) -> Result<Self> {
        if let Some(p) = explicit {
            return Self::load(p);
        }
        if let Some(env_path) = std::env::var_os("MT_GRAPH") {
            return Self::load(Path::new(&env_path));
        }
        Self::load_embedded()
    }

    /// Effective graph "as the user sees it", merging the shipped curriculum
    /// and user overlay. This is the entrypoint for scheduler, tree, and state
    /// queries.
    ///
    /// Conflict resolution rule: an overlay lesson, quiz, or tombstone
    /// always overrides a built-in item with the same ID. Tombstones
    /// override everything.
    pub async fn load_for_path(conn: &Connection, graph_dir: Option<&Path>) -> Result<Self> {
        let mut g = Self::load_default(graph_dir)?;
        let overlay = crate::overlay::load(conn).await?;
        g.apply_overlay(overlay);
        Ok(g)
    }

    /// Returns targets and the transitive closure of their prerequisites
    /// as a set of atoms.
    pub fn reachable_atoms(&self, targets: &[String]) -> HashSet<String> {
        let mut out: HashSet<String> = HashSet::new();
        let mut stack: Vec<String> = targets.to_vec();
        while let Some(id) = stack.pop() {
            let Some(c) = self.by_id.get(&id) else {
                continue;
            };
            if !c.children_ids.is_empty() {
                // Expand cluster to atoms and prereqs
                stack.extend(c.children_ids.iter().cloned());
                stack.extend(c.prerequisites.iter().cloned());
                continue;
            }
            if !out.insert(id.clone()) {
                continue;
            }
            stack.extend(c.prerequisites.iter().cloned());
        }
        out
    }

    /// Resolve each input ID to atoms, deduplicated and in first-seen
    /// order (children before their cluster's later siblings):
    ///
    /// - an atom (leaf node) maps to itself
    /// - a cluster (non-leaf node) expands to all its atomic descendants
    ///
    /// Errors if an ID maps to no atom (`UnknownId`) or names an empty
    /// cluster (`EmptyCluster`).
    pub fn expand_to_atoms(&self, ids: &[String]) -> Result<Vec<String>> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut out: Vec<String> = Vec::new();

        for id in ids {
            let before = out.len();
            match self.by_id.get(id) {
                Some(c) if c.children_ids.is_empty() => {
                    if seen.insert(id.clone()) {
                        out.push(id.clone());
                    }
                }
                Some(_) => {
                    self.collect_atomic_descendants(id, &mut seen, &mut out);
                    if out.len() == before {
                        return Err(Error::EmptyCluster(id.clone()));
                    }
                }
                None => return Err(Error::UnknownId(id.clone())),
            }
        }
        Ok(out)
    }

    fn collect_atomic_descendants(
        &self,
        id: &str,
        seen: &mut HashSet<String>,
        out: &mut Vec<String>,
    ) {
        let Some(c) = self.by_id.get(id) else { return };
        if c.children_ids.is_empty() {
            if seen.insert(id.to_string()) {
                out.push(id.to_string());
            }
        } else {
            for child in &c.children_ids {
                self.collect_atomic_descendants(child, seen, out);
            }
        }
    }

    /// Validate `id` resolves to an atom in the merged graph.
    /// Returns `AtomNotFound` if missing and `NotAtom` if id is a cluster.
    pub fn atom(&self, id: &str) -> Result<&FlatConcept> {
        let c = self
            .by_id
            .get(id)
            .ok_or_else(|| Error::AtomNotFound(id.to_string()))?;
        if !c.children_ids.is_empty() {
            return Err(Error::NotAtom(id.to_string()));
        }
        Ok(c)
    }

    /// Return the parent atom and `Quiz` for a given quiz ID.
    /// Validates that the quiz & atom exist.
    pub fn quiz(&self, quiz_id: &str) -> Result<(&FlatConcept, &Quiz)> {
        let atom_id = crate::answer::atom_from_quiz_id(quiz_id)
            .ok_or_else(|| Error::UnknownId(quiz_id.to_string()))?;
        let atom = self
            .by_id
            .get(&atom_id)
            .ok_or_else(|| Error::AtomNotFound(atom_id))?;
        let q = atom
            .quizzes
            .iter()
            .find(|q| q.id == quiz_id)
            .ok_or_else(|| Error::UnknownId(quiz_id.to_string()))?;
        Ok((atom, q))
    }

    /// Apply the user overlay to this graph in place. See
    /// [`Graph::load_for_path`] for the conflict-resolution contract.
    fn apply_overlay(&mut self, overlay: crate::overlay::Overlay) {
        for (atom_id, entry) in overlay.atoms {
            let Some(c) = self.by_id.get_mut(&atom_id) else {
                // Atom is not in the shipped graph and should be skipped.
                // A future graph version may add it, at which point the
                // overlay starts taking effect; or the user can clean
                // up the overlay manually.
                continue;
            };
            if entry.lesson.is_some() {
                c.lesson = entry.lesson;
            }

            // Quizzes: overlay entries replace shipped entries with the
            // same id (amend), and are appended otherwise (added).
            for overlay_quiz in entry.quizzes {
                match c.quizzes.iter_mut().find(|q| q.id == overlay_quiz.id) {
                    Some(existing) => *existing = overlay_quiz,
                    None => c.quizzes.push(overlay_quiz),
                }
            }
            // Tombstones beat both shipped and overlay-authored quizzes:
            // an answered quiz's `QuizAnswered` events stay in the log
            // for audit, but the merged view drops it.
            if !entry.removed.is_empty() {
                c.quizzes.retain(|q| !entry.removed.contains(&q.id));
            }
        }
    }
}

fn flatten(c: Concept, by_id: &mut HashMap<String, FlatConcept>) {
    let children = c.children;
    let children_ids: Vec<String> = children.iter().map(|x| x.id.clone()).collect();
    let id = c.id;
    by_id.insert(
        id.clone(),
        FlatConcept {
            id,
            name: c.name,
            description: c.description,
            prerequisites: c.prerequisites,
            children_ids,
            lesson: c.lesson,
            quizzes: c.quizzes,
        },
    );
    for child in children {
        flatten(child, by_id);
    }
}

// ── Validation ──────────────────────────────────────────────────────

#[derive(Debug)]
pub struct CheckIssue {
    pub area: Option<String>,
    pub node: Option<String>,
    pub message: String,
}

#[derive(Debug)]
pub struct CheckReport {
    pub areas: usize,
    pub clusters: usize,
    pub atoms: usize,
    pub max_depth: usize,
    pub prereq_edges: usize,
    pub issues: Vec<CheckIssue>,
}

#[allow(clippy::too_many_lines)]
pub fn run_check(graph_dir: Option<&Path>) -> Result<CheckReport> {
    let manifest = load_manifest_default(graph_dir)?;

    let mut report = CheckReport {
        areas: manifest.areas.len(),
        clusters: 0,
        atoms: 0,
        max_depth: 0,
        prereq_edges: 0,
        issues: Vec::new(),
    };

    let mut id_to_area: BTreeMap<String, String> = BTreeMap::new();
    let mut area_trees: Vec<(ManifestArea, u32, Vec<Concept>)> = Vec::new();

    eprintln!("loaded manifest: {} areas", manifest.areas.len());

    for entry in &manifest.areas {
        let raw = match load_area_default(graph_dir, &entry.file) {
            Ok(a) => a,
            Err(e) => {
                report.issues.push(CheckIssue {
                    area: Some(entry.slug.clone()),
                    node: None,
                    message: e.to_string(),
                });
                continue;
            }
        };

        if raw.area != entry.slug {
            report.issues.push(CheckIssue {
                area: Some(entry.slug.clone()),
                node: None,
                message: format!(
                    "area slug '{}' in file does not match manifest '{}'",
                    raw.area, entry.slug
                ),
            });
        }
        if raw.prefix != entry.prefix {
            report.issues.push(CheckIssue {
                area: Some(entry.slug.clone()),
                node: None,
                message: format!(
                    "prefix '{}' in file does not match manifest '{}'",
                    raw.prefix, entry.prefix
                ),
            });
        }

        let sv = raw.schema_version;
        match sv {
            1 => {
                if raw.topics.is_none() {
                    report.issues.push(CheckIssue {
                        area: Some(entry.slug.clone()),
                        node: None,
                        message: "schema_version=1 missing `topics:`".into(),
                    });
                }
                if raw.children.is_some() {
                    report.issues.push(CheckIssue {
                        area: Some(entry.slug.clone()),
                        node: None,
                        message: "schema_version=1 should not have `children:`".into(),
                    });
                }
            }
            2 => {
                if raw.children.is_none() {
                    report.issues.push(CheckIssue {
                        area: Some(entry.slug.clone()),
                        node: None,
                        message: "schema_version=2 missing `children:`".into(),
                    });
                }
                if raw.topics.is_some() {
                    report.issues.push(CheckIssue {
                        area: Some(entry.slug.clone()),
                        node: None,
                        message: "schema_version=2 should not have `topics:`".into(),
                    });
                }
            }
            v => {
                report.issues.push(CheckIssue {
                    area: Some(entry.slug.clone()),
                    node: None,
                    message: format!("unknown schema_version: {v}"),
                });
            }
        }

        let prefix = raw.prefix.clone();
        let area_slug = entry.slug.clone();
        let concepts = raw.into_concepts();

        let mut area_clusters = 0;
        let mut area_atoms = 0;
        for root in &concepts {
            walk_validate(
                root,
                None,
                &prefix,
                &area_slug,
                1,
                &mut id_to_area,
                &mut report,
                &mut area_clusters,
                &mut area_atoms,
            );
        }

        report.clusters += area_clusters;
        report.atoms += area_atoms;

        eprintln!(
            "  loaded {area_slug:30} [{prefix:<4}] schema_v{sv}  \
             clusters={area_clusters:>3} atoms={area_atoms:>4}"
        );

        area_trees.push((entry.clone(), sv, concepts));
    }

    for (_entry, _sv, concepts) in &area_trees {
        for root in concepts {
            check_prereqs_resolve(root, &id_to_area, &mut report);
        }
    }

    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for (_entry, _sv, concepts) in &area_trees {
        for root in concepts {
            collect_prereq_edges(root, &mut adj);
        }
    }
    detect_cycles(&id_to_area, &adj, &mut report);
    check_orphans(&area_trees, &adj, &id_to_area, &mut report);

    Ok(report)
}

/// Flag atoms that no other concept lists as a prerequisite. A
/// prerequisite reference to a cluster covers all its descendant
/// atoms (mirroring how the scheduler expands cluster prereqs in
/// `Graph::reachable_atoms`). Atoms marked `terminal: true` opt out.
fn check_orphans(
    area_trees: &[(ManifestArea, u32, Vec<Concept>)],
    adj: &HashMap<String, Vec<String>>,
    id_to_area: &BTreeMap<String, String>,
    report: &mut CheckReport,
) {
    let mut referenced: HashSet<&str> = HashSet::new();
    for prereqs in adj.values() {
        for p in prereqs {
            referenced.insert(p.as_str());
        }
    }

    let mut orphans: Vec<(String, String, String)> = Vec::new();
    for (_entry, _sv, concepts) in area_trees {
        for root in concepts {
            collect_orphan_atoms(root, &referenced, false, id_to_area, &mut orphans);
        }
    }

    orphans.sort_by(|a, b| a.0.cmp(&b.0));
    for (id, name, area) in orphans {
        report.issues.push(CheckIssue {
            area: Some(area),
            node: Some(id),
            message: format!("orphan atom '{name}'"),
        });
    }
}

fn collect_orphan_atoms(
    n: &Concept,
    referenced: &HashSet<&str>,
    ancestor_referenced: bool,
    id_to_area: &BTreeMap<String, String>,
    out: &mut Vec<(String, String, String)>,
) {
    let covered = ancestor_referenced || referenced.contains(n.id.as_str());
    if n.is_atom() && !n.terminal && !covered {
        let area = id_to_area.get(&n.id).cloned().unwrap_or_default();
        out.push((n.id.clone(), n.name.clone(), area));
    }
    for c in &n.children {
        collect_orphan_atoms(c, referenced, covered, id_to_area, out);
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_validate(
    n: &Concept,
    parent_id: Option<&str>,
    prefix: &str,
    area_slug: &str,
    depth: usize,
    id_to_area: &mut BTreeMap<String, String>,
    report: &mut CheckReport,
    clusters: &mut usize,
    atoms: &mut usize,
) {
    if n.is_atom() {
        *atoms += 1;
    } else {
        *clusters += 1;
    }
    if depth > report.max_depth {
        report.max_depth = depth;
    }

    let parts: Vec<&str> = n.id.split('.').collect();
    if parts.is_empty() || parts[0] != prefix {
        report.issues.push(CheckIssue {
            area: Some(area_slug.to_string()),
            node: Some(n.id.clone()),
            message: format!("id does not start with area prefix '{prefix}'"),
        });
    }
    for seg in parts.iter().skip(1) {
        let valid = !seg.is_empty()
            && seg.chars().all(|c| c.is_ascii_digit())
            && !(seg.len() > 1 && seg.starts_with('0'))
            && *seg != "0";
        if !valid {
            report.issues.push(CheckIssue {
                area: Some(area_slug.to_string()),
                node: Some(n.id.clone()),
                message: format!("id segment '{seg}' is not a positive integer (no leading zeros)"),
            });
        }
    }

    if let Some(p) = parent_id {
        let expected_prefix = format!("{p}.");
        if let Some(suffix) = n.id.strip_prefix(&expected_prefix) {
            if suffix.contains('.') {
                report.issues.push(CheckIssue {
                    area: Some(area_slug.to_string()),
                    node: Some(n.id.clone()),
                    message: format!("id should extend parent '{p}' by exactly one segment"),
                });
            }
        } else {
            report.issues.push(CheckIssue {
                area: Some(area_slug.to_string()),
                node: Some(n.id.clone()),
                message: format!("id does not extend parent '{p}'"),
            });
        }
    }

    if let Some(prev_area) = id_to_area.insert(n.id.clone(), area_slug.to_string()) {
        report.issues.push(CheckIssue {
            area: Some(area_slug.to_string()),
            node: Some(n.id.clone()),
            message: format!("duplicate id (also in area '{prev_area}')"),
        });
    }

    for child in &n.children {
        walk_validate(
            child,
            Some(&n.id),
            prefix,
            area_slug,
            depth + 1,
            id_to_area,
            report,
            clusters,
            atoms,
        );
    }
}

fn check_prereqs_resolve(
    n: &Concept,
    id_to_area: &BTreeMap<String, String>,
    report: &mut CheckReport,
) {
    for p in &n.prerequisites {
        report.prereq_edges += 1;
        if !id_to_area.contains_key(p) {
            report.issues.push(CheckIssue {
                area: id_to_area.get(&n.id).cloned(),
                node: Some(n.id.clone()),
                message: format!("prerequisite '{p}' references unknown id"),
            });
        }
    }
    for c in &n.children {
        check_prereqs_resolve(c, id_to_area, report);
    }
}

fn collect_prereq_edges(n: &Concept, adj: &mut HashMap<String, Vec<String>>) {
    let entry = adj.entry(n.id.clone()).or_default();
    for p in &n.prerequisites {
        entry.push(p.clone());
    }
    for c in &n.children {
        collect_prereq_edges(c, adj);
    }
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Color {
    White,
    Gray,
    Black,
}

fn cycle_dfs(
    node: &str,
    adj: &HashMap<String, Vec<String>>,
    color: &mut HashMap<String, Color>,
    stack: &mut Vec<String>,
    cycles: &mut Vec<Vec<String>>,
) {
    color.insert(node.to_string(), Color::Gray);
    stack.push(node.to_string());

    let neighbors = adj.get(node).cloned().unwrap_or_default();
    for n in &neighbors {
        let c = color.get(n).copied();
        match c {
            Some(Color::White) => cycle_dfs(n, adj, color, stack, cycles),
            Some(Color::Gray) => {
                if let Some(start) = stack.iter().position(|s| s == n) {
                    let mut path: Vec<String> = stack[start..].to_vec();
                    path.push(n.clone());
                    cycles.push(path);
                }
            }
            _ => {}
        }
    }

    color.insert(node.to_string(), Color::Black);
    stack.pop();
}

fn detect_cycles(
    id_to_area: &BTreeMap<String, String>,
    adj: &HashMap<String, Vec<String>>,
    report: &mut CheckReport,
) {
    let mut color: HashMap<String, Color> = id_to_area
        .keys()
        .map(|k| (k.clone(), Color::White))
        .collect();
    let mut cycles: Vec<Vec<String>> = Vec::new();
    let ids: Vec<String> = id_to_area.keys().cloned().collect();
    for id in &ids {
        if color.get(id).copied() == Some(Color::White) {
            cycle_dfs(id, adj, &mut color, &mut Vec::new(), &mut cycles);
        }
    }
    for c in cycles {
        report.issues.push(CheckIssue {
            area: None,
            node: c.first().cloned(),
            message: format!("prerequisite cycle: {}", c.join(" -> ")),
        });
    }
}

impl CheckReport {
    pub fn print(&self) {
        println!();
        println!("Statistics");
        println!("  areas:        {}", self.areas);
        println!("  clusters:     {}", self.clusters);
        println!("  atoms:        {}", self.atoms);
        println!("  total nodes:  {}", self.clusters + self.atoms);
        println!("  max depth:    {}", self.max_depth);
        println!("  prereq edges: {}", self.prereq_edges);

        if self.issues.is_empty() {
            println!();
            println!("graph check passed.");
        } else {
            println!();
            println!("Issues ({}):", self.issues.len());
            for i in &self.issues {
                let prefix = match (&i.area, &i.node) {
                    (Some(a), Some(n)) => format!("{a}/{n}"),
                    (Some(a), None) => a.clone(),
                    (None, Some(n)) => n.clone(),
                    _ => "(graph)".to_string(),
                };
                println!("  {prefix}: {}", i.message);
            }
            println!();
            println!("graph check FAILED.");
        }
    }
}
