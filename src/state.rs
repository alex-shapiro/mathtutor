//! `mt state`: summary report combining path data, graph state, and
//! the event log.

use std::collections::HashSet;
use std::path::Path;

use crate::event_log;
use crate::graph::Graph;
use crate::path::{PathError, load_path, resolve_id};

pub fn cmd_state(explicit_id: Option<&str>, graph_dir: &Path) -> Result<(), PathError> {
    let id = resolve_id(explicit_id)?;
    let p = load_path(&id)?;
    let g = Graph::load(graph_dir)?;

    let total = p.target_atoms.len();
    let learned = p
        .target_atoms
        .iter()
        .filter(|a| g.by_id.get(a.as_str()).is_some_and(|c| c.lesson.is_some()))
        .count();
    let pct = if total > 0 { learned * 100 / total } else { 0 };

    let events = event_log::load(&id)?;
    let target_set: HashSet<&str> = p.target_atoms.iter().map(String::as_str).collect();
    let most_recent = events
        .iter()
        .rev()
        .find(|e| {
            matches!(e.kind, event_log::EventKind::LessonAuthored)
                && e.atom.as_deref().is_some_and(|a| target_set.contains(a))
        })
        .and_then(|e| e.atom.clone());

    let next = g.first_untaught_in(p.target_atoms.iter().map(String::as_str));

    println!("{:13}{}", "path:", p.id);
    println!("{:13}{}", "goal:", p.goal);
    println!(
        "{:13}{}",
        "created:",
        p.created_at.format("%Y-%m-%dT%H:%M:%SZ")
    );
    println!("{:13}{total}", "targets:");
    println!("{:13}{learned} / {total} ({pct}%)", "learned:");
    print_atom_line("most recent:", most_recent.as_deref(), &g);
    print_atom_line("next:", next.as_deref(), &g);
    Ok(())
}

fn print_atom_line(label: &str, atom_id: Option<&str>, g: &Graph) {
    match atom_id {
        Some(id) => {
            let name = g.by_id.get(id).map_or("", |c| c.name.as_str());
            println!("{label:13}{id} — {name}");
        }
        None => println!("{label:13}—"),
    }
}
