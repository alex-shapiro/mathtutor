//! Learning path progress summary

use std::collections::HashSet;
use std::io::{self, Write};
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
use crate::scheduler;
use crate::subpath;
use crate::types::Strategy;

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
    conn: &Connection,
    explicit_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<()> {
    let s = compute_state(conn, explicit_id, graph_dir).await?;
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
    conn: &Connection,
    explicit_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<StateSummary> {
    let id = resolve_id(conn, explicit_id).await?;
    let p = load_path(conn, &id).await?;
    let g = Graph::load_for_path(conn, graph_dir).await?;
    let progress = PathProgress::load(conn, &id).await?;
    let due = cards::due_quizzes(conn, &id, Utc::now()).await?;

    let subpath_ids = subpath::load(conn, &id).await?;
    let target_atoms = p.resolve_targets(&g)?;

    let reachable = g.reachable_atoms(&target_atoms);
    let complete = complete_set(&g, &reachable, &progress);
    let (targets, reach) = counters(&target_atoms, &reachable, &complete, &progress);

    let updated_at = latest_event_ts(conn, &id).await?.unwrap_or(p.created_at);
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
    let most_recent = most_recent_completed_target(conn, &id, &g, &target_atoms, &complete)
        .await?
        .map(atom_ref_of);

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
async fn most_recent_completed_target<'a>(
    conn: &Connection,
    path_id: &str,
    g: &'a Graph,
    targets: &[String],
    complete: &HashSet<String>,
) -> Result<Option<&'a FlatConcept>> {
    let completed: Vec<&FlatConcept> = targets
        .iter()
        .filter(|a| complete.contains(a.as_str()))
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
    Ok(completed.into_iter().max_by_key(|c| {
        c.quizzes
            .iter()
            .map(|q| first_correct[q.id.as_str()])
            .max()
            .expect("complete atom has three quizzes")
    }))
}

/// `SQLite`'s default `SQLITE_MAX_VARIABLE_NUMBER` is 999; we bind one
/// placeholder for `path_id` plus one per `quiz_id`, so cap each chunk
/// at 900 ids for headroom.
const MAX_IN_PARAMS: usize = 900;

async fn load_first_correct(
    conn: &Connection,
    path_id: &str,
    quiz_ids: &[&str],
) -> Result<std::collections::HashMap<String, DateTime<Utc>>> {
    let mut out = std::collections::HashMap::with_capacity(quiz_ids.len());
    for chunk in quiz_ids.chunks(MAX_IN_PARAMS) {
        load_first_correct_chunk(conn, path_id, chunk, &mut out).await?;
    }
    Ok(out)
}

async fn load_first_correct_chunk(
    conn: &Connection,
    path_id: &str,
    quiz_ids: &[&str],
    out: &mut std::collections::HashMap<String, DateTime<Utc>>,
) -> Result<()> {
    let placeholders = vec!["?"; quiz_ids.len()].join(",");
    // rating > 1 implies a real quiz answer
    let sql = format!(
        "SELECT quiz_id, MIN(ts) FROM events \
         WHERE path_id = ? AND kind = 'quiz_answered' AND rating > 1 \
           AND quiz_id IN ({placeholders}) \
         GROUP BY quiz_id"
    );
    let params: Vec<libsql::Value> = std::iter::once(libsql::Value::from(path_id))
        .chain(quiz_ids.iter().map(|&q| libsql::Value::from(q)))
        .collect();
    let mut rows = conn.query(&sql, params).await?;
    while let Some(row) = rows.next().await? {
        let quiz_id: String = row.get(0)?;
        let ts: String = row.get(1)?;
        out.insert(quiz_id, db::parse_ts(&ts)?);
    }
    Ok(())
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
