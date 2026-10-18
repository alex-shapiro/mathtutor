//! Learning path progress summary

use std::collections::HashSet;
use std::io::{self, Write};
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::Result;
use crate::db::Db;
use crate::graph::{FlatConcept, Graph};
use crate::path::PathFile;
use crate::progress::PathProgress;
use crate::scheduler;
use crate::subpath;
use crate::types::Strategy;
use crate::view::{PathState, Want};

/// Per-path progress snapshot returned by `compute_state`.
#[derive(Debug, Serialize)]
pub struct StateSummary {
    pub path: String,
    pub goal: String,
    pub strategy: Strategy,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub targets: TargetProgress,
    /// Prerequisite-graph coverage. Omitted under the top-down strategy,
    /// where prerequisites are not required and the count would mislead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reachable: Option<ReachProgress>,
    /// The active top-down subpath's remaining (incomplete) atoms, in
    /// order. Empty when none is set or all are complete.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subpath: Vec<AtomRef>,
    pub past_due: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub most_recent: Option<AtomRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<AtomRef>,
}

/// Completion of the learning path's explicit target atoms
#[derive(Debug, Serialize)]
pub struct TargetProgress {
    pub total: usize,
    pub learned: usize,
    pub learned_pct: usize,
}

/// Completion across learning path targets & prerequisites
#[derive(Debug, Serialize)]
pub struct ReachProgress {
    pub total: usize,
    pub taught: usize,
    pub learned: usize,
}

#[derive(Debug, Serialize)]
pub struct AtomRef {
    pub id: String,
    pub name: String,
}

/// # Panics
/// Panics on stdout write failure (e.g. broken pipe), matching `println!`.
pub async fn cmd_path_state(
    db: &Db,
    explicit_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<()> {
    let s = compute_state(db, explicit_id, graph_dir).await?;
    write_state(&mut io::stdout(), &s).expect("write to stdout");
    Ok(())
}

/// Render the human-readable summary to `w`. Splits the formatting
/// from the print path so callers (e.g. tests) can capture output.
pub fn write_state<W: Write>(w: &mut W, s: &StateSummary) -> io::Result<()> {
    writeln!(w, "{:13}{}", "path:", s.path)?;
    writeln!(w, "{:13}{}", "goal:", s.goal)?;
    writeln!(w, "{:13}{}", "strategy:", s.strategy)?;
    writeln!(
        w,
        "{:13}{}",
        "created:",
        s.created_at.format("%Y-%m-%dT%H:%M:%SZ")
    )?;
    writeln!(
        w,
        "{:13}{}",
        "updated:",
        s.updated_at.format("%Y-%m-%dT%H:%M:%SZ")
    )?;
    writeln!(
        w,
        "{:13}{} / {} learned ({}%)",
        "targets:", s.targets.learned, s.targets.total, s.targets.learned_pct
    )?;
    if let Some(reach) = &s.reachable {
        writeln!(
            w,
            "{:13}{} atoms ({} with lesson, {} learned)",
            "reachable:", reach.total, reach.taught, reach.learned
        )?;
    }
    if !s.subpath.is_empty() {
        let route = s
            .subpath
            .iter()
            .map(|a| a.id.as_str())
            .collect::<Vec<_>>()
            .join(" → ");
        writeln!(w, "{:13}{route}", "subpath:")?;
    }
    writeln!(w, "{:13}{}", "past due:", s.past_due)?;
    write_atom_line(w, "most recent:", s.most_recent.as_ref())?;
    write_atom_line(w, "next:", s.next.as_ref())?;
    Ok(())
}

pub async fn compute_state(
    db: &Db,
    explicit_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<StateSummary> {
    let view = db.snapshot(&Want::path(explicit_id)).await?;
    let ps = view.path()?;
    let p = ps.file.clone();
    let g = view.graph(graph_dir)?;
    let progress = ps.progress();
    let due = ps.due(Utc::now());

    let subpath_ids = subpath::resolved(&p, &g)?;
    let target_atoms = p.resolve_targets(&g)?;

    let reachable = g.reachable_atoms(&target_atoms);
    let complete = complete_set(&g, &reachable, &progress);
    let (targets, reach) = counters(&target_atoms, &reachable, &complete, &progress);

    let updated_at = p.updated_at;
    let next = scheduler::next_action(
        &g,
        &p,
        &target_atoms,
        &progress,
        &due,
        &subpath_ids,
        scheduler::NextMode::Default,
    )
    .atom_id()
    .and_then(|id| atom_ref(&g, id));
    let most_recent =
        most_recent_completed_target(ps, &g, &target_atoms, &complete).map(atom_ref_of);

    // Prerequisites aren't required under top-down, so the reachable-graph
    // denominator would mislead; report the subpath instead.
    let reachable = match p.strategy {
        Strategy::BottomUp => Some(reach),
        Strategy::TopDown => None,
    };
    // Only the atoms still to teach on the subpath — completed ones have
    // already drained out of the route.
    let subpath = subpath_ids
        .iter()
        .filter(|a| !scheduler::is_atom_complete(&g, &progress, a))
        .filter_map(|a| atom_ref(&g, a))
        .collect();

    Ok(StateSummary {
        path: p.id.clone(),
        goal: p.goal,
        strategy: p.strategy,
        created_at: p.created_at,
        updated_at,
        targets,
        reachable,
        subpath,
        past_due: due.len(),
        most_recent,
        next,
    })
}

/// Count completed targets and completed reachable atoms. Resolves the
/// path's stored targets to atoms against `g` (see
/// [`PathFile::resolve_targets`]) before counting.
pub fn compute_progress(
    g: &Graph,
    p: &PathFile,
    progress: &PathProgress,
) -> Result<(TargetProgress, ReachProgress)> {
    let targets = p.resolve_targets(g)?;
    let reachable = g.reachable_atoms(&targets);
    let complete = complete_set(g, &reachable, progress);
    Ok(counters(&targets, &reachable, &complete, progress))
}

fn complete_set(
    g: &Graph,
    reachable: &HashSet<String>,
    progress: &PathProgress,
) -> HashSet<String> {
    reachable
        .iter()
        .filter(|a| scheduler::is_atom_complete(g, progress, a))
        .cloned()
        .collect()
}

fn counters(
    targets: &[String],
    reachable: &HashSet<String>,
    complete: &HashSet<String>,
    progress: &PathProgress,
) -> (TargetProgress, ReachProgress) {
    let total = targets.len();
    let learned = targets
        .iter()
        .filter(|a| complete.contains(a.as_str()))
        .count();
    let learned_pct = (learned * 100).checked_div(total).unwrap_or(0);
    let reach_taught = reachable
        .iter()
        .filter(|a| progress.lesson_taught(a))
        .count();
    (
        TargetProgress {
            total,
            learned,
            learned_pct,
        },
        ReachProgress {
            total: reachable.len(),
            taught: reach_taught,
            learned: complete.len(),
        },
    )
}

/// Target whose three first-correct answers are the most recent;
/// `None` when no target is fully complete.
fn most_recent_completed_target<'a>(
    ps: &PathState,
    g: &'a Graph,
    targets: &[String],
    complete: &HashSet<String>,
) -> Option<&'a FlatConcept> {
    targets
        .iter()
        .filter(|a| complete.contains(a.as_str()))
        .filter_map(|a| g.by_id.get(a.as_str()))
        .max_by_key(|c| {
            c.quizzes
                .iter()
                .filter_map(|q| ps.cards.get(&q.id)?.first_correct_at)
                .max()
        })
}

fn atom_ref(g: &Graph, id: &str) -> Option<AtomRef> {
    g.by_id.get(id).map(atom_ref_of)
}

fn atom_ref_of(c: &FlatConcept) -> AtomRef {
    AtomRef {
        id: c.id.clone(),
        name: c.name.clone(),
    }
}

fn write_atom_line<W: Write>(w: &mut W, label: &str, atom: Option<&AtomRef>) -> io::Result<()> {
    match atom {
        Some(a) => writeln!(w, "{label:13}{} — {}", a.id, a.name),
        None => writeln!(w, "{label:13}—"),
    }
}
