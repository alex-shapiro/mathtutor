//! Prerequisite tree of a learning path for `mt path tree`.

use std::collections::HashSet;
use std::io::{self, Write};

use serde::Serialize;

use crate::db::Db;
use crate::graph::{FlatConcept, Graph};
use crate::progress::{AtomStatus, PathCompletion, PathProgress, ReachProgress, TargetProgress};
use crate::scheduler::{self, PathPlan};
use crate::types::Strategy;
use crate::view::ViewRequest;
use crate::{Error, Result};

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
    pub status: Option<AtomStatus>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<TreeNode>,
}

/// Print the prerequisite tree for `explicit_id` or the latest path as text for `mt path tree`.
pub async fn cmd_path_tree(db: &Db, explicit_id: Option<&str>, depth: Option<u32>) -> Result<()> {
    let tree = compute_tree(db, explicit_id, depth).await?;
    write_tree(&mut io::stdout().lock(), &tree).map_err(Error::Stdout)
}

/// Compute the prerequisite tree for `explicit_id` or the latest path, to at most `depth` levels below each area.
pub async fn compute_tree(
    db: &Db,
    explicit_id: Option<&str>,
    depth: Option<u32>,
) -> Result<TreeView> {
    let view = db.snapshot(&ViewRequest::path(explicit_id)).await?;
    let plan = PathPlan::of(&view)?;
    let (p, g, progress) = (&plan.state.row, &plan.graph, &plan.progress);
    let target_atoms = &plan.targets;
    let subpath = &plan.subpath;
    let completion = PathCompletion::of(g, target_atoms, &plan.reachable, progress);
    let next = plan
        .next_action(scheduler::NextMode::Default)
        .atom_id()
        .map(String::from);

    let builder = NodeBuilder {
        g,
        progress,
        spine: build_spine(g, &plan.reachable),
        targets: target_atoms.iter().map(String::as_str).collect(),
        subpath: subpath.iter().map(String::as_str).collect(),
        depth,
    };
    let areas = g
        .areas
        .iter()
        .filter_map(|area| {
            let nodes: Vec<TreeNode> = g
                .by_id
                .get(&area.prefix)
                .into_iter()
                .flat_map(|root| &root.children_ids)
                .filter(|id| builder.spine.contains(*id))
                .filter_map(|id| g.by_id.get(id))
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
        subpath: subpath.clone(),
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
            status: is_atom.then(|| AtomStatus::of(self.progress, c)),
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
        "{:13}{} atoms ({} taught, {} learned)",
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
        while let Some(parent) = g.parent(id) {
            spine.insert(parent.id.clone());
            id = &parent.id;
        }
    }
    spine
}
