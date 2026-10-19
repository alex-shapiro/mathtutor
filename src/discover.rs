//! Read-only curriculum lookup (`mt graph show` and `mt graph list`).

use std::path::Path;

use serde::Serialize;

use crate::db::Db;
use crate::graph::{self, FlatConcept, Graph, Manifest};
use crate::progress::PathProgress;
use crate::scheduler;
use crate::view::{PathState, Want};
use crate::{Error, Result};

pub async fn cmd_graph_show(
    db: &Db,
    id: &str,
    path_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<()> {
    let (g, progress) = graph_for(db, path_id, graph_dir).await?;
    let mut view = show_view(&g, id)?;
    if let Some(progress) = progress {
        enrich_show_view(&mut view, &g, &progress, id);
    }
    emit(&view)
}

/// Loads the merged graph and path progress when `path_id` is provided, or the base curriculum graph otherwise.
pub async fn graph_for(
    db: &Db,
    path_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<(std::sync::Arc<Graph>, Option<PathProgress>)> {
    let Some(pid) = path_id else {
        return Ok((std::sync::Arc::new(Graph::load_default(graph_dir)?), None));
    };
    let view = db.snapshot(&Want::path(Some(pid))).await?;
    let progress = view.path().map(PathState::progress).unwrap_or_default();
    Ok((view.graph(graph_dir)?, Some(progress)))
}

pub async fn cmd_graph_list(
    db: &Db,
    id: Option<&str>,
    path_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<()> {
    let manifest = graph::load_manifest_default(graph_dir)?;
    let (g, progress) = graph_for(db, path_id, graph_dir).await?;
    let mut view = list_view(&g, &manifest, id)?;
    if let Some(progress) = progress {
        enrich_list_view(&mut view, &g, &progress);
    }
    emit(&view)
}

pub fn show_view(g: &Graph, id: &str) -> Result<ShowView> {
    let c = g
        .by_id
        .get(id)
        .ok_or_else(|| Error::UnknownId(id.to_string()))?;
    Ok(ShowView::Concept(concept_view(g, c)))
}

pub fn list_view(g: &Graph, manifest: &Manifest, id: Option<&str>) -> Result<ListView> {
    let Some(id) = id else {
        return Ok(ListView::Areas(areas_view(manifest)));
    };
    let c = g
        .by_id
        .get(id)
        .ok_or_else(|| Error::UnknownId(id.to_string()))?;
    Ok(ListView::Children(children_list_view(g, c)))
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum ShowView {
    Concept(ConceptView),
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum ListView {
    Areas(AreasView),
    Children(ChildrenListView),
}

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<AtomStatus>,
}

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
