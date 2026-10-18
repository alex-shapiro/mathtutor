//! `mt path syllabus`: forward-looking preview of upcoming lesson topics.
//!
//! `syllabus` lists atoms whose lessons haven't been taught yet, in the order
//! the scheduler will teach them. Bottom-up walks the prerequisite graph;
//! top-down lists the subpath's remaining atoms then the targets.
//! Lesson bodies are deliberately omitted: this is a roadmap, not a reader.

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

#[derive(Debug, Serialize)]
pub struct SyllabusView {
    pub schema_version: u32,
    pub path: String,
    pub goal: String,
    /// Total upcoming-untaught atoms reachable from the path's targets.
    /// `atoms.len() < total_remaining` whenever `n` truncates the view.
    pub total_remaining: usize,
    pub atoms: Vec<SyllabusAtom>,
}

#[derive(Debug, Serialize)]
pub struct SyllabusAtom {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

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

/// Build a `SyllabusView` for the given path. Pure read-only — shared by
/// the CLI (AYML) and the MCP server (JSON).
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

/// Walk the path's targets in scheduler order — prereqs first, then the
/// target itself — and collect every reachable atom whose lesson hasn't
/// been taught in this path yet. The order matches what `mt path next`
/// would emit if every quiz answered correctly on the first try.
///
/// "Taught" means a `LessonTaught` or `LessonAuthored` event was logged. Atoms whose lesson has been taught
/// but whose quizzes are still pending fall out of the syllabus — they
/// are in-progress, not upcoming.
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

/// Top-down upcoming order: the subpath's remaining (untaught) atoms
/// first — the route the learner chose back to a target — then the path's
/// untaught targets. Prerequisites are not planned under top-down, so none
/// are walked; the subpath is the only place they appear.
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

/// Append `id` to `out` if it's a not-yet-seen leaf atom whose lesson
/// hasn't been taught. Clusters are skipped (top-down lists atoms only).
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
