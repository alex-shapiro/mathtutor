//! Upcoming untaught lesson preview (`mt path syllabus`).

use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;

use crate::db::Db;
use crate::graph::Graph;
use crate::progress::PathProgress;
use crate::subpath;
use crate::types::Strategy;
use crate::view::Want;
use crate::{Error, Result};

/// Preview of upcoming untaught atoms for a learning path.
#[derive(Debug, Serialize)]
pub struct SyllabusView {
    pub schema_version: u32,
    pub path: String,
    pub goal: String,
    /// Total count of upcoming untaught atoms.
    pub total_remaining: usize,
    pub atoms: Vec<SyllabusAtom>,
}

/// Summary of an upcoming atom in the syllabus.
#[derive(Debug, Serialize)]
pub struct SyllabusAtom {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Print the upcoming syllabus as AYML (`mt path syllabus`).
pub async fn cmd_path_syllabus(
    db: &Db,
    explicit_id: Option<&str>,
    n: usize,
    graph_dir: Option<&Path>,
) -> Result<()> {
    let view = compute_syllabus(db, explicit_id, n, graph_dir).await?;
    let text = ayml::to_string(&view).map_err(|e| Error::AymlSerialize(e.to_string()))?;
    print!("{text}");
    Ok(())
}

/// Compute up to `n` upcoming untaught atoms for `explicit_id` (or the latest path).
pub async fn compute_syllabus(
    db: &Db,
    explicit_id: Option<&str>,
    n: usize,
    graph_dir: Option<&Path>,
) -> Result<SyllabusView> {
    let view = db.snapshot(&Want::path(explicit_id)).await?;
    let ps = view.path()?;
    let p = ps.file.clone();
    let g = view.graph(graph_dir)?;
    let progress = ps.progress();
    let targets = p.resolve_targets(&g)?;

    let upcoming = match p.strategy {
        Strategy::BottomUp => upcoming_atoms(&g, &targets, &progress),
        Strategy::TopDown => {
            let subpath = subpath::resolved(&p, &g)?;
            upcoming_top_down(&g, &targets, &progress, &subpath)
        }
    };
    let total_remaining = upcoming.len();
    let atoms: Vec<SyllabusAtom> = upcoming
        .into_iter()
        .take(n)
        .filter_map(|aid| {
            g.by_id.get(&aid).map(|c| SyllabusAtom {
                id: c.id.clone(),
                name: c.name.clone(),
                description: c.description.clone(),
            })
        })
        .collect();

    Ok(SyllabusView {
        schema_version: 1,
        path: p.id,
        goal: p.goal,
        total_remaining,
        atoms,
    })
}

/// Return untaught atoms reachable from `targets` in bottom-up traversal order.
pub fn upcoming_atoms(g: &Graph, targets: &[String], progress: &PathProgress) -> Vec<String> {
    let mut visited = HashSet::new();
    let mut out = Vec::new();
    for target in targets {
        collect_untaught(g, progress, target, &mut visited, &mut out);
    }
    out
}

fn collect_untaught(
    g: &Graph,
    progress: &PathProgress,
    id: &str,
    visited: &mut HashSet<String>,
    out: &mut Vec<String>,
) {
    if !visited.insert(id.to_string()) {
        return;
    }
    let Some(c) = g.by_id.get(id) else { return };
    for prereq in &c.prerequisites {
        collect_untaught(g, progress, prereq, visited, out);
    }
    if !c.children_ids.is_empty() {
        for child in &c.children_ids {
            collect_untaught(g, progress, child, visited, out);
        }
        return;
    }
    if !progress.lesson_taught(id) {
        out.push(id.to_string());
    }
}

/// Return untaught atoms from `subpath` followed by `targets` in top-down order.
pub fn upcoming_top_down(
    g: &Graph,
    targets: &[String],
    progress: &PathProgress,
    subpath: &[String],
) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for id in subpath.iter().chain(targets) {
        push_upcoming_leaf(g, progress, id, &mut seen, &mut out);
    }
    out
}

fn push_upcoming_leaf(
    g: &Graph,
    progress: &PathProgress,
    id: &str,
    seen: &mut HashSet<String>,
    out: &mut Vec<String>,
) {
    if !seen.insert(id.to_string()) {
        return;
    }
    let Some(c) = g.by_id.get(id) else { return };
    if !c.children_ids.is_empty() {
        return;
    }
    if !progress.lesson_taught(id) {
        out.push(id.to_string());
    }
}
