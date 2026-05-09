//! `mt state`: summary report combining path data, graph state, and
//! the event log.

use std::collections::HashSet;
use std::path::Path;

use crate::event_log;
use crate::graph::Graph;
use crate::path::{PathError, load_path, path_dir, resolve_id};

pub fn cmd_state(explicit_id: Option<&str>, graph_dir: &Path) -> Result<(), PathError> {
    let id = resolve_id(explicit_id)?;
    let p = load_path(&id)?;
    let g = Graph::load(graph_dir)?;
    let location = path_dir(&id)?;
    let events = event_log::load(&id)?;

    let total = p.target_atoms.len();
    let target_set: HashSet<&str> = p.target_atoms.iter().map(String::as_str).collect();

    // "Learned" = a target atom for which a `lesson_authored` event has
    // fired in *this* path's log. The canonical graph may carry lessons
    // authored in prior sessions; those don't count toward this path's
    // progress.
    let mut taught: HashSet<&str> = HashSet::new();
    let mut most_recent: Option<&str> = None;
    for e in &events {
        if matches!(e.kind, event_log::EventKind::LessonAuthored)
            && let Some(a) = e.atom.as_deref()
            && target_set.contains(a)
        {
            taught.insert(a);
            most_recent = Some(a);
        }
    }
    let learned = taught.len();
    let pct = if total > 0 { learned * 100 / total } else { 0 };

    let updated = events.last().map_or(p.created_at, |e| e.ts);
    let next = g.first_untaught_in(p.target_atoms.iter().map(String::as_str));

    println!("{:13}{}", "path:", p.id);
    println!("{:13}{}", "location:", location.display());
    println!("{:13}{}", "goal:", p.goal);
    println!(
        "{:13}{}",
        "created:",
        p.created_at.format("%Y-%m-%dT%H:%M:%SZ")
    );
    println!("{:13}{}", "updated:", updated.format("%Y-%m-%dT%H:%M:%SZ"));
    println!("{:13}{total}", "targets:");
    println!("{:13}{learned} / {total} ({pct}%)", "learned:");
    print_atom_line("most recent:", most_recent, &g);
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
