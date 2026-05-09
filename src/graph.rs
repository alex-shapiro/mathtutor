//! Curriculum graph: AYML-backed types, loader, and `mt graph check`.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::types::{Difficulty, QuizType};

// ── Errors ──────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum LoadError {
    #[error("io: {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parse: {path}: {message}")]
    Parse { path: PathBuf, message: String },
}

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
pub struct AreaFileRaw {
    pub schema_version: u32,
    pub area: String,
    pub prefix: String,
    pub summary: String,
    pub why_for_dl: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cross_references: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topics: Option<Vec<TopicRaw>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<NodeRaw>>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TopicRaw {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    pub leaves: Vec<LeafRaw>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct LeafRaw {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prerequisites: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lesson: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quizzes: Option<Vec<QuizRaw>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relevant_for: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub difficulty: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct NodeRaw {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prerequisites: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<NodeRaw>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lesson: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quizzes: Option<Vec<QuizRaw>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relevant_for: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub difficulty: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct QuizRaw {
    pub id: String,
    pub difficulty: Difficulty,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<QuizType>,
    pub question: String,
    pub answer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rubric: Option<String>,
}

// ── Unified concept tree ────────────────────────────────────────────

/// Normalized concept node — used for validation and scheduling regardless of schema.
#[derive(Debug, Clone)]
pub struct Concept {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub prerequisites: Vec<String>,
    pub children: Vec<Concept>,
    pub lesson: Option<String>,
    pub quizzes: Vec<Quiz>,
}

#[derive(Debug, Clone)]
pub struct Quiz {
    pub id: String,
    pub difficulty: Difficulty,
    pub kind: Option<QuizType>,
    pub question: String,
    pub answer: String,
    pub rubric: Option<String>,
}

impl From<QuizRaw> for Quiz {
    fn from(q: QuizRaw) -> Self {
        Quiz {
            id: q.id,
            difficulty: q.difficulty,
            kind: q.kind,
            question: q.question,
            answer: q.answer,
            rubric: q.rubric,
        }
    }
}

impl Concept {
    pub fn is_atom(&self) -> bool {
        self.children.is_empty()
    }
}

impl AreaFileRaw {
    pub fn into_concepts(self) -> Vec<Concept> {
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
                            quizzes: l
                                .quizzes
                                .unwrap_or_default()
                                .into_iter()
                                .map(Quiz::from)
                                .collect(),
                        })
                        .collect(),
                    lesson: None,
                    quizzes: Vec::new(),
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
        quizzes: n
            .quizzes
            .unwrap_or_default()
            .into_iter()
            .map(Quiz::from)
            .collect(),
    }
}

// ── Loaders ─────────────────────────────────────────────────────────

pub fn load_manifest(path: &Path) -> Result<Manifest, LoadError> {
    let reader = open_reader(path)?;
    ayml::from_reader(reader).map_err(|e| LoadError::Parse {
        path: path.to_path_buf(),
        message: e.to_string(),
    })
}

pub fn load_area(path: &Path) -> Result<AreaFileRaw, LoadError> {
    let reader = open_reader(path)?;
    ayml::from_reader(reader).map_err(|e| LoadError::Parse {
        path: path.to_path_buf(),
        message: e.to_string(),
    })
}

fn open_reader(path: &Path) -> Result<BufReader<File>, LoadError> {
    let file = File::open(path).map_err(|e| LoadError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(BufReader::new(file))
}

// ── Flat graph (for scheduler / `mt new`) ──────────────────────────

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
    pub fn load(graph_dir: &Path) -> Result<Self, LoadError> {
        let manifest = load_manifest(&graph_dir.join("manifest.ayml"))?;
        let mut by_id = HashMap::new();
        for entry in &manifest.areas {
            let raw = load_area(&graph_dir.join(&entry.file))?;
            for c in raw.into_concepts() {
                flatten(c, &mut by_id);
            }
        }
        Ok(Self { by_id })
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

pub fn run_check(graph_dir: &Path) -> Result<CheckReport, LoadError> {
    let manifest_path = graph_dir.join("manifest.ayml");
    let manifest = load_manifest(&manifest_path)?;

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
        let area_path = graph_dir.join(&entry.file);
        let raw = match load_area(&area_path) {
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
            "  loaded {:30} [{:<4}] schema_v{}  clusters={:>3} atoms={:>4}",
            area_slug, prefix, sv, area_clusters, area_atoms
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

    Ok(report)
}

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
        if !n.id.starts_with(&expected_prefix) {
            report.issues.push(CheckIssue {
                area: Some(area_slug.to_string()),
                node: Some(n.id.clone()),
                message: format!("id does not extend parent '{p}'"),
            });
        } else {
            let suffix = &n.id[expected_prefix.len()..];
            if suffix.contains('.') {
                report.issues.push(CheckIssue {
                    area: Some(area_slug.to_string()),
                    node: Some(n.id.clone()),
                    message: format!("id should extend parent '{p}' by exactly one segment"),
                });
            }
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
