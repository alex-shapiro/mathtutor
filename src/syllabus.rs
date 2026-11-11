//! Upcoming untaught lesson preview for `mt path syllabus`.

use serde::Serialize;

use crate::db::Db;
use crate::graph::Graph;
use crate::progress::PathProgress;
use crate::scheduler::{self, PathPlan};
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

/// Print the upcoming syllabus as AYML for `mt path syllabus`.
pub async fn cmd_path_syllabus(db: &Db, explicit_id: Option<&str>, n: usize) -> Result<()> {
    let view = compute_syllabus(db, explicit_id, n).await?;
    let text = ayml::to_string(&view).map_err(|e| Error::AymlSerialize(e.to_string()))?;
    print!("{text}");
    Ok(())
}

/// Compute up to `n` upcoming untaught atoms for `explicit_id` or the latest path.
pub async fn compute_syllabus(
    db: &Db,
    explicit_id: Option<&str>,
    n: usize,
) -> Result<SyllabusView> {
    let view = db.snapshot(&Want::path(explicit_id)).await?;
    let plan = PathPlan::of(&view)?;
    let g = &plan.graph;
    let strategy = plan.state.file.strategy;
    let upcoming = upcoming(g, strategy, &plan.targets, &plan.subpath, &plan.progress);
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
        path: plan.state.file.id.clone(),
        goal: plan.state.file.goal.clone(),
        total_remaining,
        atoms,
    })
}

/// Return the atoms of a path with untaught lessons, in teaching order.
pub fn upcoming(
    g: &Graph,
    strategy: Strategy,
    targets: &[String],
    subpath: &[String],
    progress: &PathProgress,
) -> Vec<String> {
    let mut order = scheduler::teaching_order(g, strategy, targets, subpath);
    order.retain(|atom| !progress.lesson_taught(atom));
    order
}
