//! `mt next` scheduler: action selection + AYML envelope output.

use std::collections::HashSet;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::answer::atom_from_quiz_id;
use crate::event_log::{self, Event, EventPayload};
use crate::graph::{self, Difficulty, FlatConcept, Graph, Quiz, QuizType};
use crate::path::{self, CardState, PathError, PathFile, Rating};

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
    // Build the envelope first — it reads the log to compute `history`,
    // and we want `history.repetitions` to count past presentations only,
    // not the one we are about to log below.
    let envelope = Envelope::build(&g, &p, action.clone());

    if let Action::PresentQuiz { quiz_id, atom_id } = &action {
        let _ = event_log::append(Event {
            ts: Utc::now(),
            kind: "quiz_presented".to_string(),
            path: p.id.clone(),
            atom: Some(atom_id.clone()),
            quiz: Some(quiz_id.clone()),
            payload: EventPayload::default(),
        });
    }

    let text = ayml::to_string(&envelope).map_err(|e| SchedulerError::Serialize(e.to_string()))?;
    print!("{text}");

    Ok(())
}

#[derive(Debug, Clone)]
pub enum Action {
    PresentQuiz {
        quiz_id: String,
        atom_id: String,
    },
    CreateLesson {
        atom_id: String,
    },
    CreateQuiz {
        atom_id: String,
        difficulty: Difficulty,
    },
    Done,
}

/// Action priority (see `DESIGN.md`):
///   1. earliest-due quiz card                  → present_quiz
///   2. untaught atom in path coverage          → create_lesson
///   3. taught atom with unfilled difficulty    → create_quiz
///   4. nothing pending                         → done
fn run_next(g: &Graph, p: &PathFile) -> Action {
    let now = Utc::now();
    if let Some((quiz_id, atom_id)) = first_due_card(g, p, now) {
        return Action::PresentQuiz { quiz_id, atom_id };
    }

    let mut visited = HashSet::new();
    for target in &p.target_atoms {
        if let Some(id) = first_untaught(g, target, &mut visited) {
            return Action::CreateLesson { atom_id: id };
        }
    }

    let mut visited = HashSet::new();
    for target in &p.target_atoms {
        if let Some((atom, diff)) = first_quiz_slot(g, target, &mut visited) {
            return Action::CreateQuiz {
                atom_id: atom,
                difficulty: diff,
            };
        }
    }

    Action::Done
}

fn first_due_card(g: &Graph, p: &PathFile, now: DateTime<Utc>) -> Option<(String, String)> {
    let mut due_cards: Vec<(&String, &CardState)> =
        p.cards.iter().filter(|(_, c)| c.due <= now).collect();
    due_cards.sort_by_key(|(_, c)| c.due);
    for (quiz_id, _) in due_cards {
        let atom_id = atom_from_quiz_id(quiz_id)?;
        let atom = g.by_id.get(&atom_id)?;
        if atom.quizzes.iter().any(|q| q.id == *quiz_id) {
            return Some((quiz_id.clone(), atom_id));
        }
        // Quiz no longer in graph (rare); skip.
    }
    None
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
) -> Option<(String, Difficulty)> {
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

fn next_missing_difficulty(quizzes: &[Quiz]) -> Option<Difficulty> {
    let has = |d: Difficulty| quizzes.iter().any(|q| q.difficulty == d);
    if !has(Difficulty::Easy) {
        Some(Difficulty::Easy)
    } else if !has(Difficulty::Medium) {
        Some(Difficulty::Medium)
    } else if !has(Difficulty::Hard) {
        Some(Difficulty::Hard)
    } else {
        None
    }
}

// ── Quiz history (derived from the event log on every present_quiz) ──

#[derive(Serialize, Default)]
struct QuizHistory {
    repetitions: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_presented_at: Option<DateTime<Utc>>,
    correct_count: u32,
    total_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    correct_pct: Option<u32>,
    recent_ratings: Vec<Rating>,
}

fn compute_quiz_history(p: &PathFile, quiz_id: &str) -> QuizHistory {
    let events = match event_log::load(&p.id) {
        Ok(es) => es,
        Err(_) => return QuizHistory::default(),
    };

    let mut h = QuizHistory::default();
    for e in &events {
        if e.quiz.as_deref() != Some(quiz_id) {
            continue;
        }
        match e.kind.as_str() {
            "quiz_presented" => {
                h.repetitions += 1;
                h.last_presented_at = Some(e.ts);
            }
            "quiz_answered" => {
                if let Some(r) = e.payload.rating {
                    h.recent_ratings.insert(0, r);
                    h.recent_ratings.truncate(10);
                    if matches!(r, Rating::Good | Rating::Easy) {
                        h.correct_count += 1;
                    }
                    h.total_count += 1;
                }
            }
            _ => {}
        }
    }
    h.correct_pct = if h.total_count > 0 {
        Some(((h.correct_count as f32 / h.total_count as f32) * 100.0).round() as u32)
    } else {
        None
    };
    h
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
    PresentQuiz(PresentQuizPayload),
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
    target_difficulty: Difficulty,
    existing_quizzes: Vec<ExistingQuizBrief>,
    prerequisites: Vec<PrereqBrief>,
    next_step: String,
}

#[derive(Serialize)]
struct PresentQuizPayload {
    atom: AtomBrief,
    quiz: QuizFull,
    history: QuizHistory,
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
    difficulty: Difficulty,
    question: String,
}

#[derive(Serialize)]
struct QuizFull {
    id: String,
    difficulty: Difficulty,
    #[serde(rename = "type")]
    kind: QuizType,
    question: String,
    answer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    rubric: Option<String>,
}

#[derive(Serialize)]
struct DonePayload {
    message: String,
}

impl Envelope {
    fn build(g: &Graph, p: &PathFile, action: Action) -> Self {
        match action {
            Action::PresentQuiz { quiz_id, atom_id } => {
                let c = g.by_id.get(&atom_id).expect("atom exists in graph");
                let q = c
                    .quizzes
                    .iter()
                    .find(|x| x.id == quiz_id)
                    .expect("quiz exists in graph");
                let atom = AtomBrief {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    description: c.description.clone(),
                    lesson: c.lesson.clone(),
                };
                let quiz = QuizFull {
                    id: q.id.clone(),
                    difficulty: q.difficulty,
                    kind: q.kind.unwrap_or_default(),
                    question: q.question.clone(),
                    answer: q.answer.clone(),
                    rubric: q.rubric.clone(),
                };
                let history = compute_quiz_history(p, &quiz_id);
                Envelope {
                    schema_version: 1,
                    action: "present_quiz".to_string(),
                    path: p.id.clone(),
                    payload: Payload::PresentQuiz(PresentQuizPayload {
                        atom,
                        quiz,
                        history,
                        next_step: format!("mt answer {quiz_id} --rating {{again|hard|good|easy}}"),
                    }),
                }
            }
            Action::CreateLesson { atom_id } => {
                let c = g.by_id.get(&atom_id).expect("atom exists in graph");
                let atom = AtomBrief {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    description: c.description.clone(),
                    lesson: None,
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
                        difficulty: q.difficulty,
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
