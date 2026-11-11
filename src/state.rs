//! Learning path progress summary for `mt path state`.

use std::io::{self, Write};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::Result;
use crate::db::Db;
use crate::graph::{FlatConcept, Graph};
use crate::progress::{PathCompletion, PathProgress, ReachProgress, TargetProgress};
use crate::scheduler::{self, PathPlan};
use crate::types::Strategy;
use crate::view::{PathState, Want};

/// Progress summary for a learning path.
#[derive(Debug, Serialize)]
pub struct StateSummary {
    pub path: String,
    pub goal: String,
    pub strategy: Strategy,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub targets: TargetProgress,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reachable: Option<ReachProgress>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subpath: Vec<AtomRef>,
    pub past_due: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub most_recent: Option<AtomRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<AtomRef>,
}

/// Identifier and display name of an atom.
#[derive(Debug, Serialize)]
pub struct AtomRef {
    pub id: String,
    pub name: String,
}

/// Print the progress summary for a learning path for `mt path state`.
///
/// # Panics
/// Panics if writing to stdout fails.
pub async fn cmd_path_state(db: &Db, explicit_id: Option<&str>) -> Result<()> {
    let s = compute_state(db, explicit_id).await?;
    write_state(&mut io::stdout(), &s).expect("write to stdout");
    Ok(())
}

/// Format `s` as human-readable text to `w`.
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
            "{:13}{} atoms ({} taught, {} learned)",
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

/// Compute the progress summary for `explicit_id` or the latest path.
pub async fn compute_state(db: &Db, explicit_id: Option<&str>) -> Result<StateSummary> {
    let view = db.snapshot(&Want::path(explicit_id)).await?;
    let plan = PathPlan::of(&view)?;
    let (ps, g, progress) = (plan.state, &plan.graph, &plan.progress);
    let p = ps.file.clone();
    let completion = PathCompletion::of(g, &plan.targets, &plan.reachable, progress);

    let next = plan
        .next_action(scheduler::NextMode::Default)
        .atom_id()
        .and_then(|id| atom_ref(g, id));
    let most_recent = most_recent_completed_target(ps, g, progress, &plan.targets).map(atom_ref_of);

    let reachable = match p.strategy {
        Strategy::BottomUp => Some(completion.reachable),
        Strategy::TopDown => None,
    };
    let subpath = plan
        .subpath
        .iter()
        .filter(|a| !plan.is_complete(a))
        .filter_map(|a| atom_ref(g, a))
        .collect();

    Ok(StateSummary {
        path: p.id.clone(),
        goal: p.goal,
        strategy: p.strategy,
        created_at: p.created_at,
        updated_at: ps.updated_at,
        targets: completion.targets,
        reachable,
        subpath,
        past_due: plan.due.len(),
        most_recent,
        next,
    })
}

fn most_recent_completed_target<'a>(
    ps: &PathState,
    g: &'a Graph,
    progress: &PathProgress,
    targets: &[String],
) -> Option<&'a FlatConcept> {
    targets
        .iter()
        .filter(|a| scheduler::is_atom_complete(g, progress, a))
        .filter_map(|a| g.by_id.get(a.as_str()))
        .max_by_key(|c| {
            c.quizzes
                .iter()
                .filter_map(|q| ps.projection.cards.get(&q.id)?.first_correct_at)
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
