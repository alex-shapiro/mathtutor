//! `mt graph show` and `mt graph list`: read-only curriculum lookup.
//! Both emit AYML on stdout so callers parse them the same way as
//! `mt path next`.

use std::path::Path;

use libsql::Connection;
use serde::Serialize;

use crate::graph::{self, FlatConcept, Graph, Manifest, ManifestArea};
use crate::progress::PathProgress;
use crate::scheduler;
use crate::{Error, Result};

// ── Commands ───────────────────────────────────────────────────────

/// Read-only `mt graph show`. When `path_id` is set, atom output is
/// enriched with per-path status (`lesson_taught`, `complete`).
pub async fn cmd_graph_show(
    conn: &Connection,
    id: &str,
    path_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<()> {
    let manifest = graph::load_manifest_default(graph_dir)?;
    let g = if path_id.is_some() {
        Graph::load_for_path(conn, graph_dir).await?
    } else {
        Graph::load_default(graph_dir)?
    };
    let mut view = show_view(&g, &manifest, id)?;
    if let Some(pid) = path_id {
        let progress = PathProgress::load(conn, pid).await?;
        enrich_show_view(&mut view, &g, &progress, id);
    }
    emit(&view)
}

/// Read-only `mt graph list`. When `path_id` is set, atom children are
/// enriched with per-path status.
pub async fn cmd_graph_list(
    conn: &Connection,
    id: Option<&str>,
    path_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<()> {
    let manifest = graph::load_manifest_default(graph_dir)?;
    let g = if path_id.is_some() {
        Graph::load_for_path(conn, graph_dir).await?
    } else {
        Graph::load_default(graph_dir)?
    };
    let mut view = list_view(&g, &manifest, id)?;
    if let Some(pid) = path_id {
        let progress = PathProgress::load(conn, pid).await?;
        enrich_list_view(&mut view, &g, &progress);
    }
    emit(&view)
}

/// Build the `mt graph show` view (atom / cluster / area) for `id`
/// against the provided graph and manifest. Pure data — used by the
/// CLI for AYML output and by the MCP `GetItem` tool for JSON.
pub fn show_view(g: &Graph, manifest: &Manifest, id: &str) -> Result<ShowView> {
    if let Some(c) = g.by_id.get(id) {
        return Ok(ShowView::Concept(concept_view(g, c)));
    }
    if let Some(area) = manifest.areas.iter().find(|a| a.prefix == id) {
        return Ok(ShowView::Concept(area_cluster_view(g, area)));
    }
    Err(Error::UnknownId(id.to_string()))
}

/// Build the `mt graph list` view (areas, area, or cluster) for `id`
/// (or `None` for the top-level area set).
pub fn list_view(g: &Graph, manifest: &Manifest, id: Option<&str>) -> Result<ListView> {
    let Some(id) = id else {
        return Ok(ListView::Areas(areas_view(manifest)));
    };
    if let Some(c) = g.by_id.get(id) {
        return Ok(ListView::Children(children_list_view(g, c)));
    }
    if let Some(area) = manifest.areas.iter().find(|a| a.prefix == id) {
        return Ok(ListView::Children(area_list_view(g, area)));
    }
    Err(Error::UnknownId(id.to_string()))
}

/// Output of [`show_view`]. AYML serialization stays flat (one record per
/// call) via `#[serde(untagged)]`.
#[derive(Serialize)]
#[serde(untagged)]
pub enum ShowView {
    Concept(ConceptView),
}

/// Output of [`list_view`].
#[derive(Serialize)]
#[serde(untagged)]
pub enum ListView {
    Areas(AreasView),
    Children(ChildrenListView),
}

// ── Views (AYML wire shapes) ──────────────────────────────────────

#[derive(Serialize)]
pub struct AreasView {
    areas: Vec<AreaSummary>,
}

#[derive(Serialize)]
pub struct AreaSummary {
    pub prefix: String,
    pub slug: String,
    pub summary: String,
}

#[derive(Serialize)]
pub struct ChildrenListView {
    pub id: String,
    pub name: String,
    pub children: Vec<ChildBrief>,
}

#[derive(Serialize)]
pub struct ChildBrief {
    pub id: String,
    pub name: String,
    pub is_atom: bool,
    /// Per-path status. Populated only when `--path P` (or MCP
    /// `path_id`) is supplied; omitted from output otherwise so the
    /// path-less shape stays byte-identical to the pre-overlay format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<AtomStatus>,
}

/// Per-path status fields shared by `mt graph show` and `mt graph
/// list` when `--path P` is set. `lesson_taught` is true once the path
/// has logged `LessonTaught` for the atom; `complete` reflects the
/// scheduler's `is_atom_complete` invariant.
#[derive(Serialize, Debug, Clone, Copy)]
pub struct AtomStatus {
    pub lesson_taught: bool,
    pub complete: bool,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum ConceptView {
    Atom(AtomView),
    Cluster(ClusterView),
}

#[derive(Serialize)]
pub struct AtomView {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub is_atom: bool,
    pub prerequisites: Vec<ChildBrief>,
    pub has_lesson: bool,
    pub quizzes: usize,
    /// Set when `--path P` is supplied — see [`ChildBrief::status`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<AtomStatus>,
}

#[derive(Serialize)]
pub struct ClusterView {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub is_atom: bool,
    pub prerequisites: Vec<ChildBrief>,
    pub children: Vec<ChildBrief>,
    pub atomic_descendants: usize,
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
            status: None,
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
        status: None,
    }
}

fn atom_status(g: &Graph, progress: &PathProgress, atom_id: &str) -> Option<AtomStatus> {
    let c = g.by_id.get(atom_id)?;
    if !c.children_ids.is_empty() {
        return None;
    }
    Some(AtomStatus {
        lesson_taught: progress.lesson_taught(atom_id),
        complete: scheduler::is_atom_complete(g, progress, atom_id),
    })
}

fn enrich_show_view(view: &mut ShowView, g: &Graph, progress: &PathProgress, id: &str) {
    if let ShowView::Concept(ConceptView::Atom(av)) = view {
        av.status = atom_status(g, progress, id);
    }
}

fn enrich_list_view(view: &mut ListView, g: &Graph, progress: &PathProgress) {
    let ListView::Children(cv) = view else {
        return;
    };
    for child in &mut cv.children {
        if child.is_atom {
            child.status = atom_status(g, progress, &child.id);
        }
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

fn emit<T: Serialize>(view: &T) -> Result<()> {
    let text = ayml::to_string(view).map_err(|e| Error::AymlSerialize(e.to_string()))?;
    print!("{text}");
    Ok(())
}
