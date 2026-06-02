//! `mt path state`: summary report combining path data, graph state,
//! and the event log.

use std::path::Path;

use chrono::{DateTime, Utc};
use libsql::Connection;
use serde::Serialize;

use crate::Result;
use crate::cards;
use crate::event_log::{self, Event};
use crate::graph::Graph;
use crate::path::{PathFile, load_path, resolve_id};
use crate::{scheduler, tree};

/// Structured snapshot used by both the CLI's human-readable output and
/// the MCP `GetState` tool's JSON.
#[derive(Debug, Serialize)]
pub struct StateSummary {
    pub path: String,
    pub goal: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub targets: TargetProgress,
    pub reachable: ReachProgress,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub most_recent: Option<AtomRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<AtomRef>,
}

/// Completion of the path's explicit target atoms.
#[derive(Debug, Serialize)]
pub struct TargetProgress {
    pub total: usize,
    pub learned: usize,
    pub learned_pct: usize,
}

/// Completion of every atom reachable from the targets (targets plus the
/// transitive closure of their prerequisites). Surfaces prerequisite
/// progress that the `targets` counter alone hides.
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

pub async fn cmd_path_state(
    conn: &Connection,
    explicit_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<()> {
    let s = compute_state(conn, explicit_id, graph_dir).await?;

    println!("{:13}{}", "path:", s.path);
    println!("{:13}{}", "goal:", s.goal);
    println!(
        "{:13}{}",
        "created:",
        s.created_at.format("%Y-%m-%dT%H:%M:%SZ")
    );
    println!(
        "{:13}{}",
        "updated:",
        s.updated_at.format("%Y-%m-%dT%H:%M:%SZ")
    );
    println!(
        "{:13}{} / {} learned ({}%)",
        "targets:", s.targets.learned, s.targets.total, s.targets.learned_pct
    );
    println!(
        "{:13}{} atoms ({} with lesson, {} learned)",
        "reachable:", s.reachable.total, s.reachable.taught, s.reachable.learned
    );
    print_atom_line("most recent:", s.most_recent.as_ref());
    print_atom_line("next:", s.next.as_ref());
    Ok(())
}

/// Build a `StateSummary` for the given path. Pure read-only — used by
/// the CLI for human output and by the MCP server for JSON tool results.
pub async fn compute_state(
    conn: &Connection,
    explicit_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<StateSummary> {
    let id = resolve_id(conn, explicit_id).await?;
    let p = load_path(conn, &id).await?;
    let g = Graph::load_for_path(conn, graph_dir).await?;
    let events = event_log::load(conn, &id).await?;
    let due = cards::due_quizzes(conn, &id, Utc::now()).await?;

    let mut most_recent_id: Option<&str> = None;
    let mut most_recent_ts: Option<DateTime<Utc>> = None;
    for atom_id in &p.target_atoms {
        if scheduler::is_atom_complete(&g, &events, atom_id)
            && let Some(ts) = scheduler::atom_completed_at(&g, &events, atom_id)
            && most_recent_ts.is_none_or(|prev| ts > prev)
        {
            most_recent_ts = Some(ts);
            most_recent_id = Some(atom_id);
        }
    }
    let (targets, reachable) = compute_progress(&g, &p, &events);
    let updated_at = events.last().map_or(p.created_at, |e| e.ts);
    let next = scheduler::next_action(&g, &p, &events, &due)
        .atom_id()
        .and_then(|id| atom_ref(&g, id));

    Ok(StateSummary {
        path: p.id.clone(),
        goal: p.goal,
        created_at: p.created_at,
        updated_at,
        targets,
        reachable,
        most_recent: most_recent_id.and_then(|id| atom_ref(&g, id)),
        next,
    })
}

/// Count target completion and reachable-atom completion. Pure; takes a
/// merged graph, the path, and the full event log. Reachable = targets
/// plus the transitive closure of their prerequisites, matching the set
/// rendered by `mt path tree`.
pub fn compute_progress(
    g: &Graph,
    p: &PathFile,
    events: &[Event],
) -> (TargetProgress, ReachProgress) {
    let total = p.target_atoms.len();
    let learned = p
        .target_atoms
        .iter()
        .filter(|a| scheduler::is_atom_complete(g, events, a))
        .count();
    let learned_pct = if total > 0 { learned * 100 / total } else { 0 };

    let reachable = tree::reachable_atoms(g, &p.target_atoms);
    let reach_total = reachable.len();
    let reach_taught = reachable
        .iter()
        .filter(|a| scheduler::lesson_taught_in_path(events, a))
        .count();
    let reach_learned = reachable
        .iter()
        .filter(|a| scheduler::is_atom_complete(g, events, a))
        .count();

    (
        TargetProgress {
            total,
            learned,
            learned_pct,
        },
        ReachProgress {
            total: reach_total,
            taught: reach_taught,
            learned: reach_learned,
        },
    )
}

fn atom_ref(g: &Graph, id: &str) -> Option<AtomRef> {
    let c = g.by_id.get(id)?;
    Some(AtomRef {
        id: c.id.clone(),
        name: c.name.clone(),
    })
}

fn print_atom_line(label: &str, atom: Option<&AtomRef>) {
    match atom {
        Some(a) => println!("{label:13}{} — {}", a.id, a.name),
        None => println!("{label:13}—"),
    }
}
