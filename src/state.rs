//! `mt state`: summary report combining path data, graph state, and
//! the event log.

use std::path::Path;

use crate::event_log;
use crate::graph::Graph;
use crate::path::{PathError, load_path, path_dir, resolve_id};
use crate::scheduler;

pub fn cmd_state(explicit_id: Option<&str>, graph_dir: &Path) -> Result<(), PathError> {
    let id = resolve_id(explicit_id)?;
    let p = load_path(&id)?;
    let g = Graph::load(graph_dir)?;
    let location = path_dir(&id)?;
    let events = event_log::load(&id)?;

    let total = p.target_atoms.len();

    // "Learned" = a target atom that's complete by scheduler rules:
    // lesson authored AND all three difficulty quizzes answered correctly
    // at least once. We pick the most recent by completion timestamp so
    // the line reflects what the user just finished, not arbitrary order.
    let mut learned = 0usize;
    let mut most_recent: Option<&str> = None;
    let mut most_recent_ts = None;
    for atom_id in &p.target_atoms {
        if scheduler::is_atom_complete(&g, &events, atom_id) {
            learned += 1;
            if let Some(ts) = scheduler::atom_completed_at(&g, &events, atom_id)
                && most_recent_ts.is_none_or(|prev| ts > prev)
            {
                most_recent_ts = Some(ts);
                most_recent = Some(atom_id);
            }
        }
    }
    let pct = if total > 0 { learned * 100 / total } else { 0 };

    let updated = events.last().map_or(p.created_at, |e| e.ts);
    let next = scheduler::next_action(&g, &p, &events)
        .atom_id()
        .map(str::to_string);

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
