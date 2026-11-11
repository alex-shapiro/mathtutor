//! Read-only curriculum lookup for `mt graph show` and `mt graph list`.

use std::sync::Arc;

use serde::Serialize;

use crate::db::Db;
use crate::graph::{Curriculum, FlatConcept, Graph, natural_id_cmp};
use crate::progress::{AtomStatus, PathProgress};
use crate::view::Want;
use crate::{Error, Result};

/// Data that a curriculum lookup reads.
#[derive(Clone, Copy)]
pub enum Lookup<'a> {
    /// The curriculum alone, read without the database.
    Curriculum(&'a Curriculum),
    /// The curriculum merged with the overlay of `db`, with the progress of path `path` if it is provided.
    Db { db: &'a Db, path: Option<&'a str> },
}

/// Print curriculum item `id` from `lookup` as AYML.
pub async fn cmd_graph_show(lookup: Lookup<'_>, id: &str) -> Result<()> {
    crate::print_ayml(&compute_item(lookup, id).await?)
}

/// Print the curriculum items under `id` from `lookup` as AYML.
pub async fn cmd_graph_list(lookup: Lookup<'_>, id: Option<&str>, recursive: bool) -> Result<()> {
    crate::print_ayml(&compute_children(lookup, id, recursive).await?)
}

/// Return curriculum item `id` from `lookup`.
pub async fn compute_item(lookup: Lookup<'_>, id: &str) -> Result<ConceptView> {
    let (g, progress) = graph_for(lookup).await?;
    let c = concept(&g, id)?;
    let mut view = concept_view(&g, c);
    if let (ConceptView::Atom(av), Some(progress)) = (&mut view, &progress) {
        av.status = atom_status(&g, progress, id);
    }
    Ok(view)
}

/// Return the children of `id` from `lookup`, or the curriculum areas if `id` is `None`, with every descendant if `recursive` is set.
pub async fn compute_children(
    lookup: Lookup<'_>,
    id: Option<&str>,
    recursive: bool,
) -> Result<ListView> {
    let (g, progress) = graph_for(lookup).await?;
    let brief = |c: &FlatConcept| ChildBrief {
        id: c.id.clone(),
        name: c.name.clone(),
        is_atom: c.children_ids.is_empty(),
        status: progress.as_ref().and_then(|p| atom_status(&g, p, &c.id)),
    };
    let roots: Vec<&str> = match id {
        Some(id) => vec![concept(&g, id)?.id.as_str()],
        None => g.areas.iter().map(|a| a.prefix.as_str()).collect(),
    };
    let descendants = if recursive {
        descendants(&g, &roots).into_iter().map(brief).collect()
    } else {
        Vec::new()
    };
    let Some(id) = id else {
        return Ok(ListView::Areas(AreasView {
            areas: g
                .areas
                .iter()
                .map(|a| AreaSummary {
                    prefix: a.prefix.clone(),
                    slug: a.slug.clone(),
                    summary: a.summary.clone(),
                })
                .collect(),
            descendants,
        }));
    };
    let c = concept(&g, id)?;
    Ok(ListView::Children(ChildrenListView {
        id: c.id.clone(),
        name: c.name.clone(),
        children: c
            .children_ids
            .iter()
            .filter_map(|cid| g.by_id.get(cid))
            .map(brief)
            .collect(),
        descendants,
    }))
}

/// Load the graph of `lookup`, with the progress of its path if it names a path.
pub async fn graph_for(lookup: Lookup<'_>) -> Result<(Arc<Graph>, Option<PathProgress>)> {
    match lookup {
        Lookup::Curriculum(curriculum) => Ok((curriculum.graph()?, None)),
        Lookup::Db { db, path: None } => Ok((db.snapshot(&Want::overlay()).await?.graph()?, None)),
        Lookup::Db {
            db,
            path: Some(path_id),
        } => {
            let view = db.snapshot(&Want::path(Some(path_id))).await?;
            let progress = view.path()?.progress();
            Ok((view.graph()?, Some(progress)))
        }
    }
}

/// Curriculum areas, or the children of a curriculum node.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum ListView {
    Areas(AreasView),
    Children(ChildrenListView),
}

#[derive(Debug, Serialize)]
pub struct AreasView {
    pub areas: Vec<AreaSummary>,
    /// Every node in the curriculum, if the lookup is recursive.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub descendants: Vec<ChildBrief>,
}

#[derive(Debug, Serialize)]
pub struct AreaSummary {
    pub prefix: String,
    pub slug: String,
    pub summary: String,
}

#[derive(Debug, Serialize)]
pub struct ChildrenListView {
    pub id: String,
    pub name: String,
    pub children: Vec<ChildBrief>,
    /// Every node below this node, if the lookup is recursive.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub descendants: Vec<ChildBrief>,
}

#[derive(Debug, Serialize)]
pub struct ChildBrief {
    pub id: String,
    pub name: String,
    pub is_atom: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<AtomStatus>,
}

/// Curriculum atom or cluster.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum ConceptView {
    Atom(AtomView),
    Cluster(ClusterView),
}

#[derive(Debug, Serialize)]
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

#[derive(Debug, Serialize)]
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

fn concept<'a>(g: &'a Graph, id: &str) -> Result<&'a FlatConcept> {
    g.by_id
        .get(id)
        .ok_or_else(|| Error::UnknownId(id.to_string()))
}

fn concept_view(g: &Graph, c: &FlatConcept) -> ConceptView {
    let briefs = |ids: &[String]| -> Vec<ChildBrief> {
        ids.iter()
            .filter_map(|id| g.by_id.get(id))
            .map(|c| ChildBrief {
                id: c.id.clone(),
                name: c.name.clone(),
                is_atom: c.children_ids.is_empty(),
                status: None,
            })
            .collect()
    };
    if c.children_ids.is_empty() {
        ConceptView::Atom(AtomView {
            id: c.id.clone(),
            name: c.name.clone(),
            description: c.description.clone(),
            is_atom: true,
            prerequisites: briefs(&c.prerequisites),
            has_lesson: c.lesson.is_some(),
            quizzes: c.quizzes.len(),
            status: None,
        })
    } else {
        ConceptView::Cluster(ClusterView {
            id: c.id.clone(),
            name: c.name.clone(),
            description: c.description.clone(),
            is_atom: false,
            prerequisites: briefs(&c.prerequisites),
            children: briefs(&c.children_ids),
            atomic_descendants: g.atomic_descendants(&c.id).len(),
        })
    }
}

fn atom_status(g: &Graph, progress: &PathProgress, atom_id: &str) -> Option<AtomStatus> {
    let c = g.by_id.get(atom_id)?;
    c.children_ids
        .is_empty()
        .then(|| AtomStatus::of(progress, c))
}

/// Return every node below `roots` in natural ID order.
fn descendants<'a>(g: &'a Graph, roots: &[&str]) -> Vec<&'a FlatConcept> {
    let mut out = Vec::new();
    let mut stack: Vec<&str> = roots.to_vec();
    while let Some(id) = stack.pop() {
        let Some(c) = g.by_id.get(id) else { continue };
        for child in &c.children_ids {
            if let Some(cc) = g.by_id.get(child) {
                out.push(cc);
                stack.push(&cc.id);
            }
        }
    }
    out.sort_by(|a, b| natural_id_cmp(&a.id, &b.id));
    out
}
