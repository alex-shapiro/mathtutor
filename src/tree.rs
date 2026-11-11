//! Prerequisite tree of a learning path for `mt path tree`.

use std::collections::HashSet;
use std::io::{self, Write};

use chrono::Utc;
use serde::Serialize;

use crate::Result;
use crate::db::Db;
use crate::graph::{FlatConcept, Graph, natural_id_cmp};
use crate::progress::{PathCompletion, PathProgress, ReachProgress, TargetProgress};
use crate::scheduler;
use crate::types::{Difficulty, Strategy};
use crate::view::Want;

/// Prerequisite tree of a learning path, grouped by curriculum area.
#[derive(Debug, Serialize)]
pub struct TreeView {
    pub path: String,
    pub goal: String,
    pub strategy: Strategy,
    pub targets: TargetProgress,
    pub reachable: ReachProgress,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subpath: Vec<String>,
    /// Atom of the next scheduled action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    pub areas: Vec<TreeArea>,
}

/// Top-level nodes of a curriculum area that the path reaches.
#[derive(Debug, Serialize)]
pub struct TreeArea {
    pub prefix: String,
    pub slug: String,
    pub nodes: Vec<TreeNode>,
}

/// Atom or cluster that the path reaches.
#[derive(Debug, Serialize)]
pub struct TreeNode {
    pub id: String,
    pub name: String,
    pub is_atom: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_target: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub on_subpath: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<TreeAtomStatus>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<TreeNode>,
}

/// Lesson and quiz status of an atom on a path.
#[derive(Debug, Serialize)]
pub struct TreeAtomStatus {
    pub lesson_taught: bool,
    pub complete: bool,
    pub easy: QuizSlot,
    pub medium: QuizSlot,
    pub hard: QuizSlot,
}

/// Status of the quiz in a difficulty slot of an atom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuizSlot {
    Missing,
    Authored,
    Correct,
}

impl TreeAtomStatus {
    /// Return the status of atom `c` of `g` in `progress`.
    pub fn of(g: &Graph, progress: &PathProgress, c: &FlatConcept) -> Self {
        let slot = |difficulty| match c.quizzes.iter().find(|q| q.difficulty == difficulty) {
            None => QuizSlot::Missing,
            Some(q) if progress.quiz_answered_correctly(&q.id) => QuizSlot::Correct,
            Some(_) => QuizSlot::Authored,
        };
        Self {
            lesson_taught: progress.lesson_taught(&c.id),
            complete: scheduler::is_atom_complete(g, progress, &c.id),
            easy: slot(Difficulty::Easy),
            medium: slot(Difficulty::Medium),
            hard: slot(Difficulty::Hard),
        }
    }

    /// Format the `[LEMH]` badge of this status.
    pub fn badge(&self) -> String {
        let slot = |s: QuizSlot, upper: char| match s {
            QuizSlot::Missing => '·',
            QuizSlot::Authored => upper.to_ascii_lowercase(),
            QuizSlot::Correct => upper,
        };
        let lesson = if self.lesson_taught { 'L' } else { '·' };
        format!(
            "[{lesson}{}{}{}]",
            slot(self.easy, 'E'),
            slot(self.medium, 'M'),
            slot(self.hard, 'H')
        )
    }
}

/// Print the prerequisite tree for `explicit_id` or the latest path as text for `mt path tree`.
///
/// # Panics
/// Panics if writing to stdout fails.
pub async fn cmd_path_tree(db: &Db, explicit_id: Option<&str>, depth: Option<u32>) -> Result<()> {
    let tree = compute_tree(db, explicit_id, depth).await?;
    write_tree(&mut io::stdout(), &tree).expect("write to stdout");
    Ok(())
}

/// Compute the prerequisite tree for `explicit_id` or the latest path, to at most `depth` levels below each area.
pub async fn compute_tree(
    db: &Db,
    explicit_id: Option<&str>,
    depth: Option<u32>,
) -> Result<TreeView> {
    let view = db.snapshot(&Want::path(explicit_id)).await?;
    let ps = view.path()?;
    let p = &ps.file;
    let g = view.graph()?;
    let progress = ps.progress();
    let subpath = crate::subpath::resolved(p, &g)?;
    let target_atoms = p.resolve_targets(&g)?;
    let completion = PathCompletion::of(&g, &target_atoms, &progress);
    let next = scheduler::next_action(
        &g,
        p,
        &target_atoms,
        &progress,
        &ps.due(Utc::now()),
        &subpath,
        scheduler::NextMode::Default,
    )
    .atom_id()
    .map(String::from);

    let builder = NodeBuilder {
        g: &g,
        progress: &progress,
        spine: build_spine(&g, &g.reachable_atoms(&target_atoms)),
        targets: target_atoms.iter().map(String::as_str).collect(),
        subpath: subpath.iter().map(String::as_str).collect(),
        depth,
    };
    let areas = g
        .areas
        .iter()
        .filter_map(|area| {
            let nodes: Vec<TreeNode> = top_level_in_area(&g, &area.prefix)
                .into_iter()
                .filter(|c| builder.spine.contains(&c.id))
                .map(|c| builder.node(c, 1))
                .collect();
            (!nodes.is_empty()).then(|| TreeArea {
                prefix: area.prefix.clone(),
                slug: area.slug.clone(),
                nodes,
            })
        })
        .collect();

    Ok(TreeView {
        path: p.id.clone(),
        goal: p.goal.clone(),
        strategy: p.strategy,
        targets: completion.targets,
        reachable: completion.reachable,
        subpath,
        next,
        areas,
    })
}

/// Path data that turns curriculum concepts into [`TreeNode`]s.
struct NodeBuilder<'a> {
    g: &'a Graph,
    progress: &'a PathProgress,
    spine: HashSet<String>,
    targets: HashSet<&'a str>,
    subpath: HashSet<&'a str>,
    depth: Option<u32>,
}

impl NodeBuilder<'_> {
    /// Return the node of `c` at `level` below its area, with its children in the spine.
    fn node(&self, c: &FlatConcept, level: u32) -> TreeNode {
        let is_atom = c.children_ids.is_empty();
        let children = if self.depth.is_some_and(|d| level >= d) {
            Vec::new()
        } else {
            c.children_ids
                .iter()
                .filter_map(|id| self.g.by_id.get(id))
                .filter(|kc| self.spine.contains(&kc.id))
                .map(|kc| self.node(kc, level + 1))
                .collect()
        };
        TreeNode {
            id: c.id.clone(),
            name: c.name.clone(),
            is_atom,
            is_target: self.targets.contains(c.id.as_str()),
            on_subpath: self.subpath.contains(c.id.as_str()),
            status: is_atom.then(|| TreeAtomStatus::of(self.g, self.progress, c)),
            children,
        }
    }
}

/// Format `t` as human-readable text to `w`.
pub fn write_tree<W: Write>(w: &mut W, t: &TreeView) -> io::Result<()> {
    writeln!(w, "{:13}{}", "path:", t.path)?;
    writeln!(w, "{:13}{}", "goal:", t.goal)?;
    writeln!(w, "{:13}{}", "strategy:", t.strategy)?;
    writeln!(
        w,
        "{:13}{} / {} learned ({}%)",
        "targets:", t.targets.learned, t.targets.total, t.targets.learned_pct
    )?;
    writeln!(
        w,
        "{:13}{} atoms ({} with lesson, {} learned)",
        "reachable:", t.reachable.total, t.reachable.taught, t.reachable.learned
    )?;
    if !t.subpath.is_empty() {
        writeln!(w, "{:13}{}", "subpath:", t.subpath.join(" → "))?;
    }
    writeln!(w)?;
    let sub_legend = if t.subpath.is_empty() {
        ""
    } else {
        "  ~=on subpath"
    };
    writeln!(w, "legend: [LEMH]  L=lesson  E/M/H=easy/medium/hard quiz")?;
    writeln!(
        w,
        "        UPPER=correct  lower=authored,not-yet-correct  ·=none"
    )?;
    writeln!(
        w,
        "        *=path target{sub_legend}  ← NEXT=scheduler's next action"
    )?;
    writeln!(w)?;
    for area in &t.areas {
        writeln!(w, "{} ({})", area.prefix, area.slug)?;
        write_nodes(w, &area.nodes, "", t.next.as_deref())?;
        writeln!(w)?;
    }
    Ok(())
}

fn write_nodes<W: Write>(
    w: &mut W,
    nodes: &[TreeNode],
    prefix: &str,
    next: Option<&str>,
) -> io::Result<()> {
    let last = nodes.len().saturating_sub(1);
    for (i, n) in nodes.iter().enumerate() {
        let is_last = i == last;
        let connector = if is_last { "└─ " } else { "├─ " };
        let Some(status) = &n.status else {
            writeln!(w, "{prefix}{connector}{} {}", n.id, n.name)?;
            let child_prefix = format!("{prefix}{}", if is_last { "   " } else { "│  " });
            write_nodes(w, &n.children, &child_prefix, next)?;
            continue;
        };
        let star = if n.is_target {
            " *"
        } else if n.on_subpath {
            " ~"
        } else {
            ""
        };
        let mark = if next == Some(n.id.as_str()) {
            "  ← NEXT"
        } else {
            ""
        };
        writeln!(
            w,
            "{prefix}{connector}{} {} {}{star}{mark}",
            status.badge(),
            n.id,
            n.name
        )?;
    }
    Ok(())
}

/// Return all atom and ancestor cluster IDs for `atoms` in `g`.
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
    out.sort_by(|a, b| natural_id_cmp(&a.id, &b.id));
    out
}

#[cfg(test)]
mod tests {
    use super::parent_id;

    #[test]
    fn parent_id_drops_last_segment() {
        assert_eq!(parent_id("la.5.4.7"), Some("la.5.4"));
        assert_eq!(parent_id("la.5"), Some("la"));
        assert_eq!(parent_id("la"), None);
    }
}
