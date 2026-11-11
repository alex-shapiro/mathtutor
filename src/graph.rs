//! Curriculum graph loader, overlay merger, and graph validator.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use include_dir::{Dir, include_dir};
use serde::{Deserialize, Serialize};

use crate::types::{Difficulty, QuizType};
use crate::{Error, Result};

static EMBEDDED_GRAPH: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/curriculum/graph");

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

#[derive(Debug, Clone)]
struct Concept {
    id: String,
    name: String,
    description: Option<String>,
    prerequisites: Vec<String>,
    children: Vec<Concept>,
    lesson: Option<String>,
    quizzes: Vec<Quiz>,
    terminal: bool,
}

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

/// Wrap top-level area concepts in a synthetic root node so that area prefixes are resolved as clusters.
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

fn parse_embedded<T: serde::de::DeserializeOwned>(name: &str) -> Result<T> {
    ayml::from_str(embedded_file_str(name)?).map_err(|e| Error::AymlParse {
        path: embedded_path(name),
        message: e.to_string(),
    })
}

/// Location of the curriculum files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CurriculumSource {
    /// The curriculum compiled into the binary.
    Embedded,
    /// A directory that holds `manifest.ayml` and the area files.
    Dir(PathBuf),
}

impl CurriculumSource {
    fn manifest(&self) -> Result<Manifest> {
        match self {
            Self::Embedded => parse_embedded("manifest.ayml"),
            Self::Dir(dir) => load_manifest(&dir.join("manifest.ayml")),
        }
    }

    fn area(&self, file: &str) -> Result<AreaFileRaw> {
        match self {
            Self::Embedded => parse_embedded(file),
            Self::Dir(dir) => load_area(&dir.join(file)),
        }
    }
}

/// Curriculum graph of a process, parsed from its source on first use.
#[derive(Debug)]
pub struct Curriculum {
    source: CurriculumSource,
    graph: OnceLock<Arc<Graph>>,
}

impl Curriculum {
    pub fn new(source: CurriculumSource) -> Self {
        Self {
            source,
            graph: OnceLock::new(),
        }
    }

    /// Return the embedded curriculum, which every caller in the process shares.
    pub fn embedded() -> Arc<Self> {
        static EMBEDDED: OnceLock<Arc<Curriculum>> = OnceLock::new();
        EMBEDDED
            .get_or_init(|| Arc::new(Self::new(CurriculumSource::Embedded)))
            .clone()
    }

    /// Return the curriculum in `dir`, or the embedded curriculum if `dir` is `None`.
    pub fn open(dir: Option<PathBuf>) -> Arc<Self> {
        match dir {
            None => Self::embedded(),
            Some(dir) => Arc::new(Self::new(CurriculumSource::Dir(dir))),
        }
    }

    pub fn source(&self) -> &CurriculumSource {
        &self.source
    }

    /// Return the curriculum graph without user overlay content.
    pub fn graph(&self) -> Result<Arc<Graph>> {
        if let Some(g) = self.graph.get() {
            return Ok(g.clone());
        }
        let g = Arc::new(Graph::load(&self.source)?);
        Ok(self.graph.get_or_init(|| g).clone())
    }

    /// Return the curriculum graph with `overlay` overrides and tombstones applied.
    pub fn merged(&self, overlay: &crate::overlay::Overlay) -> Result<Arc<Graph>> {
        let base = self.graph()?;
        if overlay.atoms.is_empty() {
            return Ok(base);
        }
        let mut g = (*base).clone();
        g.apply_overlay(overlay);
        Ok(Arc::new(g))
    }
}

/// Flattened curriculum graph, keyed by concept ID.
#[derive(Debug, Clone, Default)]
pub struct Graph {
    pub by_id: HashMap<String, FlatConcept>,
    /// Curriculum areas in manifest order.
    pub areas: Vec<ManifestArea>,
    /// ID of the parent of each concept that has a parent, keyed by concept ID.
    parent_ids: HashMap<String, String>,
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
    /// Parse the curriculum graph from `source`.
    pub fn load(source: &CurriculumSource) -> Result<Self> {
        let manifest = source.manifest()?;
        let mut by_id = HashMap::new();
        for entry in &manifest.areas {
            let raw = source.area(&entry.file)?;
            flatten(area_root_concept(entry, raw.into_concepts()), &mut by_id);
        }
        Ok(Self::new(by_id, manifest.areas))
    }

    /// Build a graph of `concepts` without areas.
    pub fn of_concepts(concepts: impl IntoIterator<Item = FlatConcept>) -> Self {
        Self::new(
            concepts.into_iter().map(|c| (c.id.clone(), c)).collect(),
            Vec::new(),
        )
    }

    fn new(by_id: HashMap<String, FlatConcept>, areas: Vec<ManifestArea>) -> Self {
        let parent_ids = by_id
            .values()
            .flat_map(|c| {
                c.children_ids
                    .iter()
                    .map(|child| (child.clone(), c.id.clone()))
            })
            .collect();
        Self {
            by_id,
            areas,
            parent_ids,
        }
    }

    /// Return the cluster or area that holds concept `id`, or `None` for an area or an unknown ID.
    pub fn parent(&self, id: &str) -> Option<&FlatConcept> {
        self.by_id.get(self.parent_ids.get(id)?)
    }

    /// Return the atoms below concept `id` in first-seen order, or `id` itself if it is an atom.
    pub fn atomic_descendants(&self, id: &str) -> Vec<String> {
        let mut atoms = Vec::new();
        self.collect_atomic_descendants(id, &mut HashSet::new(), &mut atoms);
        atoms
    }

    /// Return the atoms that atom `atom_id` requires directly, in first-seen order.
    ///
    /// The list holds the atoms of the prerequisites of the atom and of each cluster that holds it, outermost cluster first.
    pub fn atomic_prerequisites(&self, atom_id: &str) -> Vec<String> {
        let mut holders = vec![atom_id];
        let mut id = atom_id;
        while let Some(parent) = self.parent(id) {
            holders.push(&parent.id);
            id = &parent.id;
        }
        let mut seen = HashSet::new();
        let mut atoms = Vec::new();
        for holder in holders.iter().rev() {
            for p in self
                .by_id
                .get(*holder)
                .into_iter()
                .flat_map(|c| &c.prerequisites)
            {
                self.collect_atomic_descendants(p, &mut seen, &mut atoms);
            }
        }
        atoms
    }

    /// Return all atomic descendants of `targets` and their transitive atomic prerequisites.
    pub fn reachable_atoms(&self, targets: &[String]) -> HashSet<String> {
        let mut out: HashSet<String> = HashSet::new();
        let mut stack: Vec<String> = targets
            .iter()
            .flat_map(|t| self.atomic_descendants(t))
            .collect();
        while let Some(atom) = stack.pop() {
            if out.insert(atom.clone()) {
                stack.extend(self.atomic_prerequisites(&atom));
            }
        }
        out
    }

    /// Expand atom and cluster IDs into a deduplicated list of atomic descendant IDs in first-seen order.
    ///
    /// Fails with [`Error::EmptyCluster`] for a cluster without atomic descendants, even if another ID covers the same atoms.
    pub fn expand_to_atoms(&self, ids: &[String]) -> Result<Vec<String>> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut out: Vec<String> = Vec::new();
        for id in ids {
            if !self.by_id.contains_key(id) {
                return Err(Error::UnknownId(id.clone()));
            }
            let atoms = self.atomic_descendants(id);
            if atoms.is_empty() {
                return Err(Error::EmptyCluster(id.clone()));
            }
            out.extend(atoms.into_iter().filter(|a| seen.insert(a.clone())));
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

    pub fn atom(&self, id: &str) -> Result<&FlatConcept> {
        let c = self
            .by_id
            .get(id)
            .ok_or_else(|| Error::UnknownId(id.to_string()))?;
        if !c.children_ids.is_empty() {
            return Err(Error::NotAtom(id.to_string()));
        }
        Ok(c)
    }

    pub fn quiz(&self, quiz_id: &str) -> Result<(&FlatConcept, &Quiz)> {
        let atom_id =
            atom_from_quiz_id(quiz_id).ok_or_else(|| Error::UnknownId(quiz_id.to_string()))?;
        let atom = self
            .by_id
            .get(atom_id)
            .ok_or_else(|| Error::UnknownId(atom_id.to_string()))?;
        let q = atom
            .quizzes
            .iter()
            .find(|q| q.id == quiz_id)
            .ok_or_else(|| Error::UnknownId(quiz_id.to_string()))?;
        Ok((atom, q))
    }

    fn apply_overlay(&mut self, overlay: &crate::overlay::Overlay) {
        for (atom_id, entry) in &overlay.atoms {
            let Some(c) = self.by_id.get_mut(atom_id) else {
                continue;
            };
            if entry.lesson.is_some() {
                c.lesson.clone_from(&entry.lesson);
            }

            for overlay_quiz in &entry.quizzes {
                match c.quizzes.iter_mut().find(|q| q.id == overlay_quiz.id) {
                    Some(existing) => existing.clone_from(overlay_quiz),
                    None => c.quizzes.push(overlay_quiz.clone()),
                }
            }
            if !entry.removed.is_empty() {
                c.quizzes.retain(|q| !entry.removed.contains(&q.id));
            }
        }
    }
}

/// Return the atom ID prefix of a quiz ID, such as `fnd.1.1.1` for `fnd.1.1.1.q3`.
pub fn atom_from_quiz_id(quiz_id: &str) -> Option<&str> {
    quiz_id.rfind(".q").map(|pos| &quiz_id[..pos])
}

/// Compare dot-separated concept IDs segment by segment, ordering numeric segments by integer value.
pub fn natural_id_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut sa, mut sb) = (a.split('.'), b.split('.'));
    loop {
        match (sa.next(), sb.next()) {
            (Some(x), Some(y)) => {
                let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(nx), Ok(ny)) => nx.cmp(&ny),
                    _ => x.cmp(y),
                };
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => return Ordering::Equal,
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
pub fn run_check(source: &CurriculumSource) -> Result<CheckReport> {
    let manifest = source.manifest()?;

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
        let raw = match source.area(&entry.file) {
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

        let mut check = AreaCheck {
            prefix: &prefix,
            slug: &area_slug,
            id_to_area: &mut id_to_area,
            report: &mut report,
            clusters: 0,
            atoms: 0,
        };
        for root in &concepts {
            check.visit(root, None, 1);
        }
        let (area_clusters, area_atoms) = (check.clusters, check.atoms);
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
    let mut by_id = HashMap::new();
    for (entry, _sv, concepts) in &area_trees {
        flatten(area_root_concept(entry, concepts.clone()), &mut by_id);
    }
    let atom_edges =
        atomic_prerequisite_edges(&Graph::new(by_id, Vec::new()), &id_to_area, &mut report);
    detect_cycles(&id_to_area, &atom_edges, &mut report);
    check_orphans(&area_trees, &adj, &id_to_area, &mut report);

    Ok(report)
}

/// Return the atomic prerequisites of each atom in `g`, and report each atom that its own prerequisites contain.
fn atomic_prerequisite_edges(
    g: &Graph,
    id_to_area: &BTreeMap<String, String>,
    report: &mut CheckReport,
) -> HashMap<String, Vec<String>> {
    let mut edges = HashMap::new();
    for (id, area) in id_to_area {
        if g.by_id.get(id).is_none_or(|c| !c.children_ids.is_empty()) {
            continue;
        }
        let mut prereqs = g.atomic_prerequisites(id);
        if let Some(pos) = prereqs.iter().position(|p| p == id) {
            prereqs.remove(pos);
            report.issues.push(CheckIssue {
                area: Some(area.clone()),
                node: Some(id.clone()),
                message:
                    "a prerequisite of this atom or of a cluster that holds it contains this atom"
                        .into(),
            });
        }
        edges.insert(id.clone(), prereqs);
    }
    edges
}

/// Report non-terminal atoms that are not referenced as prerequisites directly or via an ancestor cluster.
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

/// Validation of the concept tree of a single area.
struct AreaCheck<'a> {
    prefix: &'a str,
    slug: &'a str,
    id_to_area: &'a mut BTreeMap<String, String>,
    report: &'a mut CheckReport,
    clusters: usize,
    atoms: usize,
}

impl AreaCheck<'_> {
    fn issue(&mut self, node: &str, message: String) {
        self.report.issues.push(CheckIssue {
            area: Some(self.slug.to_string()),
            node: Some(node.to_string()),
            message,
        });
    }

    /// Count `n` and its descendants, record their areas, and report each malformed or duplicate ID.
    fn visit(&mut self, n: &Concept, parent_id: Option<&str>, depth: usize) {
        if n.is_atom() {
            self.atoms += 1;
        } else {
            self.clusters += 1;
        }
        self.report.max_depth = self.report.max_depth.max(depth);

        let parts: Vec<&str> = n.id.split('.').collect();
        if parts.is_empty() || parts[0] != self.prefix {
            let prefix = self.prefix;
            self.issue(
                &n.id,
                format!("id does not start with area prefix '{prefix}'"),
            );
        }
        for seg in parts.iter().skip(1) {
            let valid = !seg.is_empty()
                && seg.chars().all(|c| c.is_ascii_digit())
                && !(seg.len() > 1 && seg.starts_with('0'))
                && *seg != "0";
            if !valid {
                self.issue(
                    &n.id,
                    format!("id segment '{seg}' is not a positive integer without leading zeros"),
                );
            }
        }

        if let Some(p) = parent_id {
            match n.id.strip_prefix(&format!("{p}.")) {
                Some(suffix) if suffix.contains('.') => self.issue(
                    &n.id,
                    format!("id should extend parent '{p}' by a single segment"),
                ),
                Some(_) => {}
                None => self.issue(&n.id, format!("id does not extend parent '{p}'")),
            }
        }

        if let Some(prev_area) = self.id_to_area.insert(n.id.clone(), self.slug.to_string()) {
            self.issue(&n.id, format!("duplicate id (also in area '{prev_area}')"));
        }

        for child in &n.children {
            self.visit(child, Some(&n.id), depth + 1);
        }
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

/// Validate the curriculum files of `source`, print the report, and fail if the report has issues (`mt graph check`).
pub fn cmd_graph_check(source: &CurriculumSource) -> Result<()> {
    let report = run_check(source)?;
    report.print();
    if report.issues.is_empty() {
        Ok(())
    } else {
        Err(Error::GraphCheckFailed)
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

#[cfg(test)]
mod tests {
    use super::{atom_from_quiz_id, natural_id_cmp};

    #[test]
    fn atom_from_quiz_id_strips_q_suffix() {
        assert_eq!(atom_from_quiz_id("fnd.1.1.1.q3"), Some("fnd.1.1.1"));
        assert_eq!(atom_from_quiz_id("no-dot-q-here"), None);
    }

    #[test]
    fn natural_id_cmp_orders_numeric_segments_by_value() {
        let mut ids = vec!["nn.1.10.1", "nn.1.2.1", "nn.1.1.2", "nn.1.1.1"];
        ids.sort_by(|a, b| natural_id_cmp(a, b));
        assert_eq!(ids, vec!["nn.1.1.1", "nn.1.1.2", "nn.1.2.1", "nn.1.10.1"]);
    }

    #[test]
    fn natural_id_cmp_groups_by_area_prefix() {
        let mut ids = vec!["nn.1.1", "la.10.1", "la.2.1"];
        ids.sort_by(|a, b| natural_id_cmp(a, b));
        assert_eq!(ids, vec!["la.2.1", "la.10.1", "nn.1.1"]);
    }

    #[test]
    fn natural_id_cmp_orders_prefix_before_its_children() {
        assert_eq!(natural_id_cmp("nn.1", "nn.1.1"), std::cmp::Ordering::Less);
    }
}
