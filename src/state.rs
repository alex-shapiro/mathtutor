//! `mt path state`: per-path progress summary.
//!
//! Reads strictly from indexed projections (path row, cards, two
//! event-table aggregates) and the merged curriculum graph — never the
//! full event log. The raw log is event-payload-heavy; state needs only
//! atom/quiz identifiers plus aggregate timestamps.

use std::path::Path;

use chrono::{DateTime, Utc};
use libsql::{Connection, params};
use serde::Serialize;

use crate::Result;
use crate::cards;
use crate::db;
use crate::graph::Graph;
use crate::path::{PathFile, load_path, resolve_id};
use crate::progress::PathProgress;
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

pub async fn compute_state(
    conn: &Connection,
    explicit_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<StateSummary> {
    let id = resolve_id(conn, explicit_id).await?;
    let p = load_path(conn, &id).await?;
    let g = Graph::load_for_path(conn, graph_dir).await?;
    let progress = PathProgress::load(conn, &id).await?;
    let due = cards::due_quizzes(conn, &id, Utc::now()).await?;

    let (targets, reachable) = compute_progress(&g, &p, &progress);
    let updated_at = latest_event_ts(conn, &id).await?.unwrap_or(p.created_at);
    let next = scheduler::next_action(&g, &p, &progress, &due)
        .atom_id()
        .and_then(|id| atom_ref(&g, id));
    let most_recent = most_recent_completed_target(conn, &id, &g, &p, &progress)
        .await?
        .and_then(|id| atom_ref(&g, &id));

    Ok(StateSummary {
        path: p.id.clone(),
        goal: p.goal,
        created_at: p.created_at,
        updated_at,
        targets,
        reachable,
        most_recent,
        next,
    })
}

/// Count target completion and reachable-atom completion. Pure; takes
/// the merged graph, the path, and a `PathProgress`. Reachable = targets
/// plus the transitive closure of their prerequisites, matching the set
/// rendered by `mt path tree`.
pub fn compute_progress(
    g: &Graph,
    p: &PathFile,
    progress: &PathProgress,
) -> (TargetProgress, ReachProgress) {
    let total = p.target_atoms.len();
    let learned = p
        .target_atoms
        .iter()
        .filter(|a| scheduler::is_atom_complete(g, progress, a))
        .count();
    let learned_pct = if total > 0 { learned * 100 / total } else { 0 };

    let reachable = tree::reachable_atoms(g, &p.target_atoms);
    let reach_total = reachable.len();
    let reach_taught = reachable
        .iter()
        .filter(|a| progress.lesson_taught(a))
        .count();
    let reach_learned = reachable
        .iter()
        .filter(|a| scheduler::is_atom_complete(g, progress, a))
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

async fn latest_event_ts(conn: &Connection, path_id: &str) -> Result<Option<DateTime<Utc>>> {
    let mut rows = conn
        .query(
            "SELECT MAX(ts) FROM events WHERE path_id = ?",
            params![path_id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let raw: Option<String> = row.get(0)?;
    raw.map(|s| db::parse_ts(&s)).transpose()
}

/// Atom whose three first-correct quiz answers landed most recently —
/// the target the learner most recently fully nailed. Returns `None`
/// when no target is fully complete.
///
/// Resolves with a single targeted SQL query against `events`: only
/// the quiz IDs belonging to completed target atoms are inspected.
async fn most_recent_completed_target(
    conn: &Connection,
    path_id: &str,
    g: &Graph,
    p: &PathFile,
    progress: &PathProgress,
) -> Result<Option<String>> {
    let mut completed_quizzes: Vec<(String, String)> = Vec::new(); // (quiz_id, atom_id)
    for atom_id in &p.target_atoms {
        if !scheduler::is_atom_complete(g, progress, atom_id) {
            continue;
        }
        let Some(c) = g.by_id.get(atom_id) else {
            continue;
        };
        for q in &c.quizzes {
            completed_quizzes.push((q.id.clone(), atom_id.clone()));
        }
    }
    if completed_quizzes.is_empty() {
        return Ok(None);
    }

    let placeholders = vec!["?"; completed_quizzes.len()].join(",");
    let sql = format!(
        "SELECT quiz_id, MIN(ts) FROM events \
         WHERE path_id = ? AND kind = 'quiz_answered' AND rating > 1 \
           AND quiz_id IN ({placeholders}) \
         GROUP BY quiz_id"
    );
    let mut params_vec: Vec<libsql::Value> = Vec::with_capacity(1 + completed_quizzes.len());
    params_vec.push(libsql::Value::from(path_id));
    for (q, _) in &completed_quizzes {
        params_vec.push(libsql::Value::from(q.as_str()));
    }
    let mut rows = conn.query(&sql, params_vec).await?;

    let mut first_correct: std::collections::HashMap<String, DateTime<Utc>> =
        std::collections::HashMap::new();
    while let Some(row) = rows.next().await? {
        let quiz_id: String = row.get(0)?;
        let ts_str: String = row.get(1)?;
        first_correct.insert(quiz_id, db::parse_ts(&ts_str)?);
    }

    // Roll up: per atom, max across its quizzes' first-correct ts → atom_completed_at.
    let mut best: Option<(String, DateTime<Utc>)> = None;
    for atom_id in &p.target_atoms {
        if !scheduler::is_atom_complete(g, progress, atom_id) {
            continue;
        }
        let Some(c) = g.by_id.get(atom_id) else {
            continue;
        };
        let mut atom_ts: Option<DateTime<Utc>> = None;
        let mut all_present = true;
        for q in &c.quizzes {
            if let Some(ts) = first_correct.get(&q.id) {
                atom_ts = Some(atom_ts.map_or(*ts, |prev| prev.max(*ts)));
            } else {
                all_present = false;
                break;
            }
        }
        if !all_present {
            continue;
        }
        if let Some(ts) = atom_ts
            && best.as_ref().is_none_or(|(_, prev)| ts > *prev)
        {
            best = Some((atom_id.clone(), ts));
        }
    }
    Ok(best.map(|(id, _)| id))
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
