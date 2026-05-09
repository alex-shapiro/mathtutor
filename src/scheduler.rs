//! `mt next` scheduler: action selection + AYML envelope output.

use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;

use crate::graph::{self, FlatConcept, Graph, Quiz};
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
    CreateQuiz { atom_id: String, difficulty: String },
    Done,
}

/// Priority (per DESIGN.md):
///   (1) due quiz card — not yet implemented
///   (2) untaught atom in path coverage         → create_lesson
///   (3) taught atom with unfilled difficulty   → create_quiz
///   (4) otherwise                               → done
fn run_next(g: &Graph, p: &PathFile) -> Action {
    // Phase 2: any untaught atom (the user's targets, walking back through prereqs)?
    let mut visited = HashSet::new();
    for target in &p.target_atoms {
        if let Some(id) = first_untaught(g, target, &mut visited) {
            return Action::CreateLesson { atom_id: id };
        }
    }

    // Phase 3: any taught atom missing a difficulty slot?
    let mut visited = HashSet::new();
    for target in &p.target_atoms {
        if let Some((atom, diff)) = first_quiz_slot(g, target, &mut visited) {
            return Action::CreateQuiz {
                atom_id: atom,
                difficulty: diff.to_string(),
            };
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

fn first_quiz_slot(
    g: &Graph,
    id: &str,
    visited: &mut HashSet<String>,
) -> Option<(String, &'static str)> {
    if !visited.insert(id.to_string()) {
        return None;
    }
    let c = g.by_id.get(id)?;
    for prereq in &c.prerequisites {
        if let Some(found) = first_quiz_slot(g, prereq, visited) {
            return Some(found);
        }
    }
    if c.children_ids.is_empty() {
        if c.lesson.is_some()
            && let Some(diff) = next_missing_difficulty(&c.quizzes)
        {
            return Some((id.to_string(), diff));
        }
    } else {
        for child_id in &c.children_ids {
            if let Some(found) = first_quiz_slot(g, child_id, visited) {
                return Some(found);
            }
        }
    }
    None
}

fn next_missing_difficulty(quizzes: &[Quiz]) -> Option<&'static str> {
    let has = |d: &str| quizzes.iter().any(|q| q.difficulty == d);
    if !has("easy") {
        Some("easy")
    } else if !has("medium") {
        Some("medium")
    } else if !has("hard") {
        Some("hard")
    } else {
        None
    }
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
    CreateQuiz(CreateQuizPayload),
    Done(DonePayload),
}

#[derive(Serialize)]
struct CreateLessonPayload {
    atom: AtomBrief,
    prerequisites: Vec<PrereqBrief>,
    next_step: String,
}

#[derive(Serialize)]
struct CreateQuizPayload {
    atom: AtomBrief,
    target_difficulty: String,
    existing_quizzes: Vec<ExistingQuizBrief>,
    prerequisites: Vec<PrereqBrief>,
    next_step: String,
}

#[derive(Serialize)]
struct AtomBrief {
    id: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lesson: Option<String>,
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
struct ExistingQuizBrief {
    id: String,
    difficulty: String,
    question: String,
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
                    lesson: None, // create_lesson never includes the to-be-authored lesson
                };
                let prerequisites = collect_prereqs(g, c);
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
            Action::CreateQuiz {
                atom_id,
                difficulty,
            } => {
                let c = g.by_id.get(&atom_id).expect("atom exists in graph");
                let atom = AtomBrief {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    description: c.description.clone(),
                    lesson: c.lesson.clone(),
                };
                let existing_quizzes: Vec<ExistingQuizBrief> = c
                    .quizzes
                    .iter()
                    .map(|q| ExistingQuizBrief {
                        id: q.id.clone(),
                        difficulty: q.difficulty.clone(),
                        question: q.question.clone(),
                    })
                    .collect();
                let prerequisites = collect_prereqs(g, c);
                let next_step = format!(
                    "mt store quiz {atom_id} --difficulty {difficulty} \
                     --question TEXT --answer TEXT [--rubric TEXT]"
                );
                Envelope {
                    schema_version: 1,
                    action: "create_quiz".to_string(),
                    path: p.id.clone(),
                    payload: Payload::CreateQuiz(CreateQuizPayload {
                        atom,
                        target_difficulty: difficulty,
                        existing_quizzes,
                        prerequisites,
                        next_step,
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

fn collect_prereqs(g: &Graph, c: &FlatConcept) -> Vec<PrereqBrief> {
    c.prerequisites
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
        .collect()
}
