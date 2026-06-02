//! `mt path state`: per-path progress summary.

use std::path::Path;

use chrono::{DateTime, Utc};
use libsql::{Connection, params};
use serde::Serialize;

use crate::Result;
use crate::cards;
use crate::db;
use crate::graph::{FlatConcept, Graph};
use crate::path::{PathFile, load_path, resolve_id};
use crate::progress::PathProgress;
use crate::{scheduler, tree};

/// Per-path progress snapshot returned by `compute_state`.
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

/// Completion across the targets plus the transitive closure of their
/// prerequisites.
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

/// Count completed targets and completed reachable atoms.
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

/// Target whose three first-correct answers are the most recent;
/// `None` when no target is fully complete.
async fn most_recent_completed_target(
    conn: &Connection,
    path_id: &str,
    g: &Graph,
    p: &PathFile,
    progress: &PathProgress,
) -> Result<Option<String>> {
    let completed: Vec<&FlatConcept> = p
        .target_atoms
        .iter()
        .filter(|a| scheduler::is_atom_complete(g, progress, a))
        .filter_map(|a| g.by_id.get(a.as_str()))
        .collect();
    if completed.is_empty() {
        return Ok(None);
    }
    let quiz_ids: Vec<&str> = completed
        .iter()
        .flat_map(|c| c.quizzes.iter().map(|q| q.id.as_str()))
        .collect();

    let first_correct = load_first_correct(conn, path_id, &quiz_ids).await?;

    // `is_atom_complete` guarantees a logged correct answer for every
    // quiz, so the SQL returns a row for each id.
    let best = completed
        .into_iter()
        .map(|c| {
            let ts = c
                .quizzes
                .iter()
                .map(|q| first_correct[q.id.as_str()])
                .max()
                .expect("complete atom has three quizzes");
            (c.id.clone(), ts)
        })
        .max_by_key(|(_, ts)| *ts);
    Ok(best.map(|(id, _)| id))
}

async fn load_first_correct(
    conn: &Connection,
    path_id: &str,
    quiz_ids: &[&str],
) -> Result<std::collections::HashMap<String, DateTime<Utc>>> {
    let placeholders = vec!["?"; quiz_ids.len()].join(",");
    // rating > 1 = any non-`Again` answer (see `types::Rating`).
    let sql = format!(
        "SELECT quiz_id, MIN(ts) FROM events \
         WHERE path_id = ? AND kind = 'quiz_answered' AND rating > 1 \
           AND quiz_id IN ({placeholders}) \
         GROUP BY quiz_id"
    );
    let mut params: Vec<libsql::Value> = Vec::with_capacity(1 + quiz_ids.len());
    params.push(libsql::Value::from(path_id));
    for q in quiz_ids {
        params.push(libsql::Value::from(*q));
    }
    let mut rows = conn.query(&sql, params).await?;
    let mut out = std::collections::HashMap::with_capacity(quiz_ids.len());
    while let Some(row) = rows.next().await? {
        let quiz_id: String = row.get(0)?;
        let ts: String = row.get(1)?;
        out.insert(quiz_id, db::parse_ts(&ts)?);
    }
    Ok(out)
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
