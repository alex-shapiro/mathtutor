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
    let reachable = reachable_atoms(&g, &p.target_atoms);
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

/// Targets plus the transitive closure of their prerequisites, returned
/// as the set of atomic concepts (no clusters). Cluster IDs may legally
/// appear as prereqs in the graph — meaning "all atoms in that cluster"
/// — so we expand any cluster encountered into its atomic descendants
/// and continue the walk from each of those.
pub fn reachable_atoms(g: &Graph, targets: &[String]) -> HashSet<String> {
    let mut out: HashSet<String> = HashSet::new();
    let mut stack: Vec<String> = targets.to_vec();
    while let Some(id) = stack.pop() {
        let Some(c) = g.by_id.get(&id) else { continue };
        if !c.children_ids.is_empty() {
            // Cluster — expand to atoms, and carry along any prereqs the
            // cluster itself declares (v2 schema allows them at any level).
            for child in &c.children_ids {
                stack.push(child.clone());
            }
            for prereq in &c.prerequisites {
                stack.push(prereq.clone());
            }
            continue;
        }
        if !out.insert(id.clone()) {
            continue;
        }
        for prereq in &c.prerequisites {
            stack.push(prereq.clone());
        }
    }
    out
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
    use std::collections::{HashMap, HashSet};

    use chrono::Utc;

    use crate::event_log::{Event, EventKind, EventPayload};
    use crate::graph::{FlatConcept, Graph, Quiz};
    use crate::progress::PathProgress;
    use crate::types::{Difficulty, Rating};

    use super::{build_spine, natural_id_key, parent_id, reachable_atoms, state_badge};

    fn cluster(id: &str, children: &[&str]) -> FlatConcept {
        FlatConcept {
            id: id.into(),
            name: id.into(),
            description: None,
            prerequisites: Vec::new(),
            children_ids: children.iter().map(|s| (*s).to_string()).collect(),
            lesson: None,
            quizzes: Vec::new(),
        }
    }

    fn atom(id: &str, prereqs: &[&str], lesson: Option<&str>, quizzes: Vec<Quiz>) -> FlatConcept {
        FlatConcept {
            id: id.into(),
            name: id.into(),
            description: None,
            prerequisites: prereqs.iter().map(|s| (*s).to_string()).collect(),
            children_ids: Vec::new(),
            lesson: lesson.map(String::from),
            quizzes,
        }
    }

    fn quiz(id: &str, difficulty: Difficulty) -> Quiz {
        Quiz {
            id: id.into(),
            difficulty,
            kind: None,
            question: "q".into(),
            answer: "a".into(),
            rubric: None,
        }
    }

    fn graph_of(concepts: Vec<FlatConcept>) -> Graph {
        let mut by_id = HashMap::new();
        for c in concepts {
            by_id.insert(c.id.clone(), c);
        }
        Graph { by_id }
    }

    fn answered(quiz_id: &str, rating: Rating) -> Event {
        Event {
            ts: Utc::now(),
            kind: EventKind::QuizAnswered,
            path: "p_test".into(),
            atom: None,
            quiz: Some(quiz_id.into()),
            payload: EventPayload {
                rating: Some(rating),
                ..Default::default()
            },
        }
    }

    fn taught(atom_id: &str) -> Event {
        Event {
            ts: Utc::now(),
            kind: EventKind::LessonTaught,
            path: "p_test".into(),
            atom: Some(atom_id.into()),
            quiz: None,
            payload: EventPayload::default(),
        }
    }

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

    #[test]
    fn reachable_includes_transitive_prereqs() {
        // tx.1.1 → la.2.2 → la.1.1 ; tx.1.1 → fnd.2.3.1 (different area).
        // All four should be reachable from a single tx.1.1 target.
        let g = graph_of(vec![
            atom("tx.1.1", &["la.2.2", "fnd.2.3.1"], None, vec![]),
            atom("la.2.2", &["la.1.1"], None, vec![]),
            atom("la.1.1", &[], None, vec![]),
            atom("fnd.2.3.1", &[], None, vec![]),
        ]);
        let reach = reachable_atoms(&g, &["tx.1.1".to_string()]);
        assert!(reach.contains("tx.1.1"));
        assert!(reach.contains("la.2.2"));
        assert!(reach.contains("la.1.1"));
        assert!(reach.contains("fnd.2.3.1"));
    }

    #[test]
    fn reachable_expands_cluster_prereqs_to_their_atoms() {
        // tx.1.1's prereq points at the cluster `la.2.2`, not a specific
        // atom. The cluster has two leaves; both must end up reachable.
        let g = graph_of(vec![
            atom("tx.1.1", &["la.2.2"], None, vec![]),
            cluster("la.2.2", &["la.2.2.1", "la.2.2.2"]),
            atom("la.2.2.1", &[], None, vec![]),
            atom("la.2.2.2", &[], None, vec![]),
        ]);
        let reach = reachable_atoms(&g, &["tx.1.1".to_string()]);
        assert!(reach.contains("tx.1.1"));
        assert!(reach.contains("la.2.2.1"));
        assert!(reach.contains("la.2.2.2"));
        // The cluster itself is not an atom and must not appear.
        assert!(!reach.contains("la.2.2"));
    }

    #[test]
    fn reachable_handles_diamond_without_looping() {
        // tx.1 → A, tx.1 → B ; A → C, B → C. C must show up exactly once.
        let g = graph_of(vec![
            atom("tx.1", &["a", "b"], None, vec![]),
            atom("a", &["c"], None, vec![]),
            atom("b", &["c"], None, vec![]),
            atom("c", &[], None, vec![]),
        ]);
        let reach = reachable_atoms(&g, &["tx.1".to_string()]);
        assert_eq!(reach.len(), 4);
    }

    #[test]
    fn build_spine_includes_atoms_and_existing_ancestors() {
        // Cluster `la.5` exists; the bare prefix `la` does not (it's an
        // area root, only in the manifest). Spine should include the
        // cluster but stop at the missing bare prefix.
        let g = graph_of(vec![
            cluster("la.5", &["la.5.4"]),
            cluster("la.5.4", &["la.5.4.7"]),
            atom("la.5.4.7", &[], None, vec![]),
        ]);
        let mut atoms = HashSet::new();
        atoms.insert("la.5.4.7".to_string());
        let spine = build_spine(&g, &atoms);
        assert!(spine.contains("la.5.4.7"));
        assert!(spine.contains("la.5.4"));
        assert!(spine.contains("la.5"));
        assert!(!spine.contains("la"));
    }

    fn progress_of(events: &[Event]) -> PathProgress {
        PathProgress::from_events(events)
    }

    #[test]
    fn state_badge_empty_when_nothing_stored() {
        let a = atom("a", &[], None, vec![]);
        assert_eq!(state_badge(&PathProgress::default(), &a), "[····]");
    }

    #[test]
    fn state_badge_lesson_slot_only_lights_up_after_taught() {
        let a = atom("a", &[], Some("body"), vec![]);
        // Body exists in the graph but no `LessonTaught` event for this
        // path yet — `L` stays unlit.
        assert_eq!(state_badge(&PathProgress::default(), &a), "[····]");
        let events = vec![taught("a")];
        assert_eq!(state_badge(&progress_of(&events), &a), "[L···]");
    }

    #[test]
    fn state_badge_lowercase_when_quiz_authored_but_unanswered() {
        let a = atom(
            "a",
            &[],
            Some("body"),
            vec![
                quiz("a.q1", Difficulty::Easy),
                quiz("a.q2", Difficulty::Medium),
            ],
        );
        let events = vec![taught("a")];
        assert_eq!(state_badge(&progress_of(&events), &a), "[Lem·]");
    }

    #[test]
    fn state_badge_uppercase_when_quiz_answered_correctly() {
        let a = atom(
            "a",
            &[],
            Some("body"),
            vec![
                quiz("a.q1", Difficulty::Easy),
                quiz("a.q2", Difficulty::Medium),
                quiz("a.q3", Difficulty::Hard),
            ],
        );
        let events = vec![
            taught("a"),
            answered("a.q1", Rating::Good),
            answered("a.q2", Rating::Easy),
            answered("a.q3", Rating::Again), // wrong → stays lowercase
        ];
        assert_eq!(state_badge(&progress_of(&events), &a), "[LEMh]");
    }
}
