//! `mt path tree`: hierarchical view of the active path.
//!
//! Renders every atom reachable from the path's targets — targets plus
//! the transitive closure of their prerequisites — at its natural cluster
//! location, walking areas in manifest order so foundations appear first.
//! Each atom shows its `[LEMH]` state badge; targets are flagged with `*`,
//! and the scheduler's next atom with `← NEXT`.

use std::collections::HashSet;
use std::path::Path;

use chrono::Utc;
use libsql::Connection;

use crate::Result;
use crate::cards;
use crate::graph::{self, FlatConcept, Graph};
use crate::path::{load_path, resolve_id};
use crate::progress::PathProgress;
use crate::scheduler;
use crate::types::Difficulty;

pub async fn cmd_path_tree(
    conn: &Connection,
    explicit_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<()> {
    let id = resolve_id(conn, explicit_id).await?;
    let p = load_path(conn, &id).await?;
    let g = Graph::load_for_path(conn, graph_dir).await?;
    let manifest = graph::load_manifest_default(graph_dir)?;
    let progress = PathProgress::load(conn, &id).await?;
    let due = cards::due_quizzes(conn, &id, Utc::now()).await?;

    let targets: HashSet<String> = p.target_atoms.iter().cloned().collect();
    let reachable = g.reachable_atoms(&p.target_atoms);
    let spine = build_spine(&g, &reachable);
    let next_atom = scheduler::next_action(&g, &p, &progress, &due)
        .atom_id()
        .map(String::from);

    let total = p.target_atoms.len();
    let target_learned = p
        .target_atoms
        .iter()
        .filter(|a| scheduler::is_atom_complete(&g, &progress, a))
        .count();
    let pct = if total > 0 {
        target_learned * 100 / total
    } else {
        0
    };
    let reach_total = reachable.len();
    let reach_taught = reachable
        .iter()
        .filter(|a| progress.lesson_taught(a))
        .count();
    let reach_learned = reachable
        .iter()
        .filter(|a| scheduler::is_atom_complete(&g, &progress, a))
        .count();
    println!("path:        {}", p.id);
    println!("goal:        {}", p.goal);
    println!("targets:     {target_learned} / {total} learned ({pct}%)");
    println!(
        "reachable:   {reach_total} atoms ({reach_taught} with lesson, {reach_learned} learned)"
    );
    println!();
    println!("legend: [LEMH]  L=lesson  E/M/H=easy/medium/hard quiz");
    println!("        UPPER=correct  lower=authored,not-yet-correct  ·=none");
    println!("        *=path target  ← NEXT=scheduler's next action");
    println!();

    for area in &manifest.areas {
        let roots: Vec<&FlatConcept> = top_level_in_area(&g, &area.prefix)
            .into_iter()
            .filter(|c| spine.contains(&c.id))
            .collect();
        if roots.is_empty() {
            continue;
        }
        println!("{} ({})", area.prefix, area.slug);
        let last = roots.len().saturating_sub(1);
        for (i, root) in roots.iter().enumerate() {
            render_node(
                &g,
                &progress,
                &spine,
                &targets,
                root,
                "",
                i == last,
                next_atom.as_deref(),
            );
        }
        println!();
    }
    Ok(())
}

/// Cluster IDs (and atom IDs) needed to root every reachable atom in the
/// hierarchy. Walks each atom up via id-prefix (`la.5.4.7` → `la.5.4` →
/// `la.5`), keeping only ancestors that exist in the graph (the area's
/// own bare prefix `la` lives in the manifest, not the graph).
pub fn build_spine<'a, I>(g: &Graph, atoms: I) -> HashSet<String>
where
    I: IntoIterator<Item = &'a String>,
{
    let mut spine: HashSet<String> = HashSet::new();
    for atom in atoms {
        spine.insert(atom.clone());
        let mut id = atom.as_str();
        while let Some(parent) = parent_id(id) {
            if g.by_id.contains_key(parent) {
                spine.insert(parent.to_string());
            }
            id = parent;
        }
    }
    spine
}

fn parent_id(id: &str) -> Option<&str> {
    id.rfind('.').map(|i| &id[..i])
}

fn top_level_in_area<'a>(g: &'a Graph, prefix: &str) -> Vec<&'a FlatConcept> {
    let mut out: Vec<&FlatConcept> = g
        .by_id
        .values()
        .filter(|c| {
            let parts: Vec<&str> = c.id.split('.').collect();
            parts.len() == 2 && parts[0] == prefix
        })
        .collect();
    // Natural-numeric sort so `tx.2` comes before `tx.10`, not after.
    out.sort_by_key(|c| natural_id_key(&c.id));
    out
}

fn natural_id_key(id: &str) -> Vec<u32> {
    id.split('.')
        .filter_map(|s| s.parse::<u32>().ok())
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn render_node(
    g: &Graph,
    progress: &PathProgress,
    spine: &HashSet<String>,
    targets: &HashSet<String>,
    c: &FlatConcept,
    prefix: &str,
    is_last: bool,
    next_atom: Option<&str>,
) {
    let connector = if is_last { "└─ " } else { "├─ " };
    let child_prefix = format!("{prefix}{}", if is_last { "   " } else { "│  " });

    if c.children_ids.is_empty() {
        let badge = state_badge(progress, c);
        let star = if targets.contains(&c.id) { " *" } else { "" };
        let mark = if next_atom == Some(c.id.as_str()) {
            "  ← NEXT"
        } else {
            ""
        };
        println!("{prefix}{connector}{badge} {} {}{star}{mark}", c.id, c.name);
        return;
    }

    println!("{prefix}{connector}{} {}", c.id, c.name);
    let kids: Vec<&FlatConcept> = c
        .children_ids
        .iter()
        .filter_map(|cid| g.by_id.get(cid))
        .filter(|kc| spine.contains(&kc.id))
        .collect();
    let last = kids.len().saturating_sub(1);
    for (i, kid) in kids.iter().enumerate() {
        render_node(
            g,
            progress,
            spine,
            targets,
            kid,
            &child_prefix,
            i == last,
            next_atom,
        );
    }
}

pub fn state_badge(progress: &PathProgress, c: &FlatConcept) -> String {
    // The `L` slot reflects "taught in this path" (per-path), not just
    // "lesson body exists in the graph" (cross-path / authored anywhere).
    let lesson = if progress.lesson_taught(&c.id) {
        'L'
    } else {
        '·'
    };
    let easy = quiz_badge(progress, c, Difficulty::Easy, 'E');
    let med = quiz_badge(progress, c, Difficulty::Medium, 'M');
    let hard = quiz_badge(progress, c, Difficulty::Hard, 'H');
    format!("[{lesson}{easy}{med}{hard}]")
}

fn quiz_badge(progress: &PathProgress, c: &FlatConcept, diff: Difficulty, upper: char) -> char {
    match c.quizzes.iter().find(|q| q.difficulty == diff) {
        None => '·',
        Some(q) => {
            if progress.quiz_answered_correctly(&q.id) {
                upper
            } else {
                upper.to_ascii_lowercase()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{natural_id_key, parent_id};

    #[test]
    fn natural_id_key_sorts_tx_2_before_tx_10() {
        // Lexicographic sort would put `tx.10` between `tx.1` and `tx.2`.
        // Natural-numeric sort must keep them in `1, 2, 10` order.
        let mut ids = vec!["tx.10", "tx.2", "tx.1"];
        ids.sort_by_key(|id| natural_id_key(id));
        assert_eq!(ids, vec!["tx.1", "tx.2", "tx.10"]);
    }

    #[test]
    fn parent_id_drops_last_segment() {
        assert_eq!(parent_id("la.5.4.7"), Some("la.5.4"));
        assert_eq!(parent_id("la.5"), Some("la"));
        assert_eq!(parent_id("la"), None);
    }
}
