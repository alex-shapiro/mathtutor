//! `mt next` scheduler: action selection + AYML envelope output.

use std::collections::HashSet;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::answer::atom_from_quiz_id;
use crate::cards;
use crate::event_log::{self, Event, EventKind};
use crate::graph::{FlatConcept, Graph};
use crate::path::{self, PathFile};
use crate::types::{Difficulty, QuizType, Rating};
use crate::{Error, Result};

const DIFFICULTIES: [Difficulty; 3] = [Difficulty::Easy, Difficulty::Medium, Difficulty::Hard];

pub fn cmd_next(path_id: Option<&str>, graph_dir: Option<&Path>) -> Result<()> {
    let id = path::resolve_id(path_id)?;
    let g = Graph::load_for_path(&id, graph_dir)?;
    let p = path::load_path(&id)?;
    let events = event_log::load(&id)?;

    let action = next_action(&g, &p, &events);
    // Build the envelope first — it reads the log to compute `history`,
    // and we want `history.repetitions` to count past presentations only,
    // not the one we are about to log below.
    let envelope = Envelope::build(&g, &p, action.clone());

    match &action {
        Action::PresentQuiz { quiz_id, atom_id } => {
            let _ = event_log::append(event_log::quiz_presented(
                p.id.clone(),
                atom_id.clone(),
                quiz_id.clone(),
            ));
        }
        Action::PresentLesson { atom_id } => {
            let _ = event_log::append(event_log::lesson_taught(p.id.clone(), atom_id.clone()));
        }
        _ => {}
    }

    let text = ayml::to_string(&envelope).map_err(|e| Error::AymlSerialize(e.to_string()))?;
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
    PresentLesson {
        atom_id: String,
    },
    CreateQuiz {
        atom_id: String,
        difficulty: Difficulty,
    },
    Done,
}

impl Action {
    pub fn atom_id(&self) -> Option<&str> {
        match self {
            Action::CreateLesson { atom_id }
            | Action::PresentLesson { atom_id }
            | Action::CreateQuiz { atom_id, .. }
            | Action::PresentQuiz { atom_id, .. } => Some(atom_id),
            Action::Done => None,
        }
    }
}

/// Action priority (see `DESIGN.md`):
///   1. earliest-due quiz card → `present_quiz`
///   2. for each path target (and its prereqs), in topo order:
///      a. no lesson yet → `create_lesson`
///      b. missing difficulty slot → `create_quiz`
///      c. quiz never answered correctly → `present_quiz`
///   3. otherwise → `done`
///
/// (2) walks per-atom: an atom isn't considered complete — and the
/// walker doesn't advance to the next atom — until its lesson is
/// stored, all three difficulty slots are filled, and each quiz has
/// at least one `quiz_answered` event with rating `good` or `easy`.
pub fn next_action(g: &Graph, p: &PathFile, events: &[Event]) -> Action {
    let now = Utc::now();
    if let Some((quiz_id, atom_id)) = first_due_card(g, events, now) {
        return Action::PresentQuiz { quiz_id, atom_id };
    }

    let mut visited = HashSet::new();
    for target in &p.target_atoms {
        if let Some(action) = next_atom_action(g, events, target, &mut visited) {
            return action;
        }
    }
    Action::Done
}

/// Walk an atom (and its prereqs / children) and return the first
/// pending action, or `None` if everything reachable is complete.
fn next_atom_action(
    g: &Graph,
    events: &[Event],
    id: &str,
    visited: &mut HashSet<String>,
) -> Option<Action> {
    if !visited.insert(id.to_string()) {
        return None;
    }
    let c = g.by_id.get(id)?;

    for prereq in &c.prerequisites {
        if let Some(action) = next_atom_action(g, events, prereq, visited) {
            return Some(action);
        }
    }

    if !c.children_ids.is_empty() {
        for child_id in &c.children_ids {
            if let Some(action) = next_atom_action(g, events, child_id, visited) {
                return Some(action);
            }
        }
        return None;
    }

    // Atom: lesson, then easy → medium → hard (each authored and answered correctly).
    if c.lesson.is_none() {
        return Some(Action::CreateLesson {
            atom_id: id.to_string(),
        });
    }
    // Lesson exists in the graph but hasn't been taught in *this* path
    // yet (e.g. authored under a previous path, or this is the first
    // walk and we haven't created/presented it yet). Surface the stored
    // body before any quiz so the user gets context.
    if !lesson_taught_in_path(events, id) {
        return Some(Action::PresentLesson {
            atom_id: id.to_string(),
        });
    }
    for diff in DIFFICULTIES {
        match c.quizzes.iter().find(|q| q.difficulty == diff) {
            None => {
                return Some(Action::CreateQuiz {
                    atom_id: id.to_string(),
                    difficulty: diff,
                });
            }
            Some(quiz) => {
                if !quiz_answered_correctly(events, &quiz.id) {
                    return Some(Action::PresentQuiz {
                        quiz_id: quiz.id.clone(),
                        atom_id: id.to_string(),
                    });
                }
            }
        }
    }
    None
}

fn first_due_card(g: &Graph, events: &[Event], now: DateTime<Utc>) -> Option<(String, String)> {
    let cards = cards::all_card_states(events).ok()?;
    let mut due: Vec<(String, DateTime<Utc>)> = cards
        .into_iter()
        .filter_map(|(quiz_id, c)| (c.due <= now).then_some((quiz_id, c.due)))
        .collect();
    due.sort_by_key(|(_, t)| *t);
    for (quiz_id, _) in due {
        let atom_id = atom_from_quiz_id(&quiz_id)?;
        let atom = g.by_id.get(&atom_id)?;
        if atom.quizzes.iter().any(|q| q.id == quiz_id) {
            return Some((quiz_id, atom_id));
        }
        // Quiz no longer in graph (rare); skip.
    }
    None
}

/// Has this quiz ever been answered with `good` or `easy`?
pub fn quiz_answered_correctly(events: &[Event], quiz_id: &str) -> bool {
    events.iter().any(|e| {
        matches!(e.kind, EventKind::QuizAnswered)
            && e.quiz.as_deref() == Some(quiz_id)
            && matches!(e.payload.rating, Some(Rating::Good | Rating::Easy))
    })
}

/// Has this atom's lesson been presented to the user during *this* path?
/// `mt store lesson` and `mt next → present_lesson` both auto-log
/// `LessonTaught` so authoring or re-presenting count as teaching.
///
/// `LessonAuthored` is accepted as an equivalent signal so that paths
/// created before `LessonTaught` existed still register correctly —
/// authoring a lesson always implies presenting it (see AGENTS.md's
/// `create_lesson` playbook).
pub fn lesson_taught_in_path(events: &[Event], atom_id: &str) -> bool {
    events.iter().any(|e| {
        matches!(e.kind, EventKind::LessonTaught | EventKind::LessonAuthored)
            && e.atom.as_deref() == Some(atom_id)
    })
}

/// "Complete" = lesson stored AND all three difficulty quizzes exist
/// AND each has at least one correct answer in the log.
pub fn is_atom_complete(g: &Graph, events: &[Event], atom_id: &str) -> bool {
    let Some(c) = g.by_id.get(atom_id) else {
        return false;
    };
    if c.lesson.is_none() {
        return false;
    }
    DIFFICULTIES.iter().all(|diff| {
        c.quizzes
            .iter()
            .find(|q| q.difficulty == *diff)
            .is_some_and(|q| quiz_answered_correctly(events, &q.id))
    })
}

/// Wall-clock time at which this atom became complete (max ts among
/// the three first-correct answers). `None` if not yet complete.
pub fn atom_completed_at(g: &Graph, events: &[Event], atom_id: &str) -> Option<DateTime<Utc>> {
    let c = g.by_id.get(atom_id)?;
    c.lesson.as_ref()?;
    let mut latest: Option<DateTime<Utc>> = None;
    for diff in DIFFICULTIES {
        let quiz = c.quizzes.iter().find(|q| q.difficulty == diff)?;
        let first_correct = events.iter().find(|e| {
            matches!(e.kind, EventKind::QuizAnswered)
                && e.quiz.as_deref() == Some(&quiz.id)
                && matches!(e.payload.rating, Some(Rating::Good | Rating::Easy))
        })?;
        latest = Some(latest.map_or(first_correct.ts, |l| l.max(first_correct.ts)));
    }
    latest
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

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn compute_quiz_history(p: &PathFile, quiz_id: &str) -> QuizHistory {
    let Ok(events) = event_log::load(&p.id) else {
        return QuizHistory::default();
    };

    let mut h = QuizHistory::default();
    for e in &events {
        if e.quiz.as_deref() != Some(quiz_id) {
            continue;
        }
        match e.kind {
            EventKind::QuizPresented => {
                h.repetitions += 1;
                h.last_presented_at = Some(e.ts);
            }
            EventKind::QuizAnswered => {
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

fn compute_lesson_history(p: &PathFile, atom_id: &str) -> LessonHistory {
    let Ok(events) = event_log::load(&p.id) else {
        return LessonHistory::default();
    };

    let mut h = LessonHistory::default();
    for e in &events {
        if e.atom.as_deref() != Some(atom_id) {
            continue;
        }
        if matches!(e.kind, EventKind::LessonTaught) {
            h.repetitions += 1;
            h.last_presented_at = Some(e.ts);
        }
    }
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
    PresentLesson(PresentLessonPayload),
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
struct PresentLessonPayload {
    atom: AtomBrief,
    reason: &'static str,
    history: LessonHistory,
    next_step: String,
}

#[derive(Serialize, Default)]
struct LessonHistory {
    repetitions: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_presented_at: Option<DateTime<Utc>>,
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
    #[allow(clippy::too_many_lines)]
    fn build(g: &Graph, p: &PathFile, action: Action) -> Self {
        match action {
            Action::PresentLesson { atom_id } => {
                let c = g.by_id.get(&atom_id).expect("atom exists in graph");
                let atom = AtomBrief {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    description: c.description.clone(),
                    lesson: c.lesson.clone(),
                };
                let history = compute_lesson_history(p, &atom_id);
                Envelope {
                    schema_version: 1,
                    action: "present_lesson".to_string(),
                    path: p.id.clone(),
                    payload: Payload::PresentLesson(PresentLessonPayload {
                        atom,
                        reason: "not_taught",
                        history,
                        next_step: "mt next".to_string(),
                    }),
                }
            }
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
