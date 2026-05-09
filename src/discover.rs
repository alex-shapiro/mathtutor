//! `mt show` and `mt list`: read-only curriculum lookup. Both emit
//! AYML on stdout so callers parse them the same way as `mt next`.

use std::path::Path;

use serde::Serialize;

use crate::graph::{self, FlatConcept, Graph, Manifest, ManifestArea};

#[derive(Debug, thiserror::Error)]
pub enum DiscoverError {
    #[error(transparent)]
    Graph(#[from] graph::LoadError),
    #[error("ayml serialize: {0}")]
    Serialize(String),
    #[error("unknown id: {0}")]
    UnknownId(String),
}

// ── Commands ───────────────────────────────────────────────────────

pub fn cmd_show(id: &str, graph_dir: &Path) -> Result<(), DiscoverError> {
    let manifest = graph::load_manifest(&graph_dir.join("manifest.ayml"))?;
    let g = Graph::load(graph_dir)?;

    if let Some(c) = g.by_id.get(id) {
        return emit(&concept_view(&g, c));
    }
    if let Some(area) = manifest.areas.iter().find(|a| a.prefix == id) {
        return emit(&area_cluster_view(&g, area));
    }
    Err(DiscoverError::UnknownId(id.to_string()))
}

pub fn cmd_list(id: Option<&str>, graph_dir: &Path) -> Result<(), DiscoverError> {
    let manifest = graph::load_manifest(&graph_dir.join("manifest.ayml"))?;
    let g = Graph::load(graph_dir)?;

    let Some(id) = id else {
        return emit(&areas_view(&manifest));
    };

    if let Some(c) = g.by_id.get(id) {
        return emit(&children_list_view(&g, c));
    }
    if let Some(area) = manifest.areas.iter().find(|a| a.prefix == id) {
        return emit(&area_list_view(&g, area));
    }
    Err(DiscoverError::UnknownId(id.to_string()))
}

// ── Views (AYML wire shapes) ──────────────────────────────────────

#[derive(Serialize)]
struct AreasView {
    areas: Vec<AreaSummary>,
}

#[derive(Serialize)]
struct AreaSummary {
    prefix: String,
    slug: String,
    summary: String,
}

#[derive(Serialize)]
struct ChildrenListView {
    id: String,
    name: String,
    children: Vec<ChildBrief>,
}

#[derive(Serialize)]
struct ChildBrief {
    id: String,
    name: String,
    is_atom: bool,
}

#[derive(Serialize)]
#[serde(untagged)]
enum ConceptView {
    Atom(AtomView),
    Cluster(ClusterView),
}

#[derive(Serialize)]
struct AtomView {
    id: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    is_atom: bool,
    prerequisites: Vec<ChildBrief>,
    has_lesson: bool,
    quizzes: usize,
}

#[derive(Serialize)]
struct ClusterView {
    id: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    is_atom: bool,
    prerequisites: Vec<ChildBrief>,
    children: Vec<ChildBrief>,
    atomic_descendants: usize,
}

// ── View builders ─────────────────────────────────────────────────

fn areas_view(manifest: &Manifest) -> AreasView {
    AreasView {
        areas: manifest
            .areas
            .iter()
            .map(|a| AreaSummary {
                prefix: a.prefix.clone(),
                slug: a.slug.clone(),
                summary: a.summary.clone(),
            })
            .collect(),
    }
}

fn area_list_view(g: &Graph, area: &ManifestArea) -> ChildrenListView {
    ChildrenListView {
        id: area.prefix.clone(),
        name: area.slug.clone(),
        children: area_top_level_children(g, &area.prefix),
    }
}

fn area_cluster_view(g: &Graph, area: &ManifestArea) -> ConceptView {
    ConceptView::Cluster(ClusterView {
        id: area.prefix.clone(),
        name: area.slug.clone(),
        description: Some(area.summary.clone()),
        is_atom: false,
        prerequisites: Vec::new(),
        children: area_top_level_children(g, &area.prefix),
        atomic_descendants: count_atoms_under_prefix(g, &area.prefix),
    })
}

fn children_list_view(g: &Graph, c: &FlatConcept) -> ChildrenListView {
    ChildrenListView {
        id: c.id.clone(),
        name: c.name.clone(),
        children: c
            .children_ids
            .iter()
            .filter_map(|cid| g.by_id.get(cid))
            .map(child_brief)
            .collect(),
    }
}

fn concept_view(g: &Graph, c: &FlatConcept) -> ConceptView {
    let prerequisites: Vec<ChildBrief> = c
        .prerequisites
        .iter()
        .filter_map(|pid| g.by_id.get(pid))
        .map(child_brief)
        .collect();

    if c.children_ids.is_empty() {
        ConceptView::Atom(AtomView {
            id: c.id.clone(),
            name: c.name.clone(),
            description: c.description.clone(),
            is_atom: true,
            prerequisites,
            has_lesson: c.lesson.is_some(),
            quizzes: c.quizzes.len(),
        })
    } else {
        let children: Vec<ChildBrief> = c
            .children_ids
            .iter()
            .filter_map(|cid| g.by_id.get(cid))
            .map(child_brief)
            .collect();
        ConceptView::Cluster(ClusterView {
            id: c.id.clone(),
            name: c.name.clone(),
            description: c.description.clone(),
            is_atom: false,
            prerequisites,
            children,
            atomic_descendants: count_atomic_descendants(g, &c.id),
        })
    }
}

// ── Helpers ───────────────────────────────────────────────────────

fn child_brief(c: &FlatConcept) -> ChildBrief {
    ChildBrief {
        id: c.id.clone(),
        name: c.name.clone(),
        is_atom: c.children_ids.is_empty(),
    }
}

fn area_top_level_children(g: &Graph, prefix: &str) -> Vec<ChildBrief> {
    let mut out: Vec<ChildBrief> = g
        .by_id
        .values()
        .filter(|c| {
            let parts: Vec<&str> = c.id.split('.').collect();
            parts.len() == 2 && parts[0] == prefix
        })
        .map(child_brief)
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn count_atoms_under_prefix(g: &Graph, prefix: &str) -> usize {
    let prefix_dot = format!("{prefix}.");
    g.by_id
        .iter()
        .filter(|(id, c)| id.starts_with(&prefix_dot) && c.children_ids.is_empty())
        .count()
}

fn count_atomic_descendants(g: &Graph, id: &str) -> usize {
    let Some(c) = g.by_id.get(id) else { return 0 };
    if c.children_ids.is_empty() {
        1
    } else {
        c.children_ids
            .iter()
            .map(|cid| count_atomic_descendants(g, cid))
            .sum()
    }
}

fn emit<T: Serialize>(view: &T) -> Result<(), DiscoverError> {
    let text = ayml::to_string(view).map_err(|e| DiscoverError::Serialize(e.to_string()))?;
    print!("{text}");
    Ok(())
}
