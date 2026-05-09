//! `mt next` scheduler: action selection + AYML envelope output.

use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;

use crate::graph::{self, Graph};
use crate::path::{self, PathError, PathFile};

#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    #[error(transparent)]
    Path(#[from] PathError),
    #[error(transparent)]
    Graph(#[from] graph::LoadError),
    #[error("ayml serialize: {0}")]
    Serialize(String),
}

pub fn cmd_next(path_id: Option<&str>, graph_dir: &Path) -> Result<(), SchedulerError> {
    let g = Graph::load(graph_dir)?;
    let id = path::resolve_id(path_id)?;
    let p = path::load_path(&id)?;

    let action = run_next(&g, &p);
    let envelope = Envelope::build(&g, &p, action);

    let text = ayml::to_string(&envelope).map_err(|e| SchedulerError::Serialize(e.to_string()))?;
    print!("{text}");

    Ok(())
}

#[derive(Debug)]
pub enum Action {
    CreateLesson { atom_id: String },
    Done,
}

fn run_next(g: &Graph, p: &PathFile) -> Action {
    let mut visited = HashSet::new();
    for target in &p.target_atoms {
        if let Some(id) = first_untaught(g, target, &mut visited) {
            return Action::CreateLesson { atom_id: id };
        }
    }
    Action::Done
}

fn first_untaught(g: &Graph, id: &str, visited: &mut HashSet<String>) -> Option<String> {
    if !visited.insert(id.to_string()) {
        return None;
    }
    let c = g.by_id.get(id)?;
    for prereq in &c.prerequisites {
        if let Some(found) = first_untaught(g, prereq, visited) {
            return Some(found);
        }
    }
    if c.children_ids.is_empty() {
        if c.lesson.is_none() {
            return Some(id.to_string());
        }
    } else {
        for child_id in &c.children_ids {
            if let Some(found) = first_untaught(g, child_id, visited) {
                return Some(found);
            }
        }
    }
    None
}

// ── AYML output shape ──────────────────────────────────────────────

#[derive(Serialize)]
struct Envelope {
    schema_version: u32,
    action: String,
    path: String,
    payload: Payload,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Payload {
    CreateLesson(CreateLessonPayload),
    Done(DonePayload),
}

#[derive(Serialize)]
struct CreateLessonPayload {
    atom: AtomBrief,
    prerequisites: Vec<PrereqBrief>,
    next_step: String,
}

#[derive(Serialize)]
struct AtomBrief {
    id: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

#[derive(Serialize)]
struct PrereqBrief {
    id: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lesson: Option<String>,
}

#[derive(Serialize)]
struct DonePayload {
    message: String,
}

impl Envelope {
    fn build(g: &Graph, p: &PathFile, action: Action) -> Self {
        match action {
            Action::CreateLesson { atom_id } => {
                let c = g.by_id.get(&atom_id).expect("atom exists in graph");
                let atom = AtomBrief {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    description: c.description.clone(),
                };
                let prerequisites: Vec<PrereqBrief> = c
                    .prerequisites
                    .iter()
                    .filter_map(|pid| {
                        let pc = g.by_id.get(pid)?;
                        Some(PrereqBrief {
                            id: pc.id.clone(),
                            name: pc.name.clone(),
                            description: pc.description.clone(),
                            lesson: pc.lesson.clone(),
                        })
                    })
                    .collect();
                Envelope {
                    schema_version: 1,
                    action: "create_lesson".to_string(),
                    path: p.id.clone(),
                    payload: Payload::CreateLesson(CreateLessonPayload {
                        atom,
                        prerequisites,
                        next_step: format!("mt store lesson {atom_id} --body TEXT"),
                    }),
                }
            }
            Action::Done => Envelope {
                schema_version: 1,
                action: "done".to_string(),
                path: p.id.clone(),
                payload: Payload::Done(DonePayload {
                    message: "Path complete.".into(),
                }),
            },
        }
    }
}
