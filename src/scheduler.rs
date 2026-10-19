//! Action scheduler and AYML output envelope for `mt path next`.

use std::collections::HashSet;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::answer::atom_from_quiz_id;
use crate::db::Db;
use crate::event_log::{self, Event, EventKind};
use crate::graph::{FlatConcept, Graph};
use crate::path::{self, PathFile};
use crate::progress::PathProgress;
use crate::subpath;
use crate::types::{Difficulty, QuizType, Rating, Strategy};
use crate::view::{PathState, Want};
use crate::{Error, Result};

const DIFFICULTIES: [Difficulty; 3] = [Difficulty::Easy, Difficulty::Medium, Difficulty::Hard];

/// Compute the next action for `path_id` and print its AYML envelope (`mt path next`).
pub async fn cmd_path_next(
    db: &Db,
    path_id: Option<&str>,
    graph_dir: Option<&Path>,
    mode: NextMode,
) -> Result<()> {
    let envelope = compute_next(db, path_id, graph_dir, mode).await?;
    let text = ayml::to_string(&envelope).map_err(|e| Error::AymlSerialize(e.to_string()))?;
    print!("{text}");
    Ok(())
}

/// Filter for [`compute_next`] action selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum NextMode {
    #[default]
    Default,
    New,
    Due,
}

impl NextMode {
    /// Convert `--new` and `--due` CLI flags into a [`NextMode`].
    pub fn from_flags(new: bool, due: bool) -> Result<Self> {
        match (new, due) {
            (true, true) => Err(Error::ConflictingFlags("--new", "--due")),
            (true, false) => Ok(Self::New),
            (false, true) => Ok(Self::Due),
            (false, false) => Ok(Self::Default),
        }
    }
}

/// Select the next action for `path_id` and append its presentation event.
pub async fn compute_next(
    db: &Db,
    path_id: Option<&str>,
    graph_dir: Option<&Path>,
    mode: NextMode,
) -> Result<Envelope> {
    db.transact(&Want::path(path_id), |view, changes| {
        let ps = view.path()?;
        let p = &ps.file;
        let g = view.graph(graph_dir)?;
        let progress = ps.progress();
        let due = ps.due(Utc::now());
        let subpath = subpath::resolved(p, &g)?;
        let targets = p.resolve_targets(&g)?;

        let action = next_action(&g, p, &targets, &progress, &due, &subpath, mode);
        // Build the envelope before appending presentation events so history reflects prior attempts.
        let envelope = Envelope::build(ps, &g, action.clone());

        match &action {
            Action::PresentQuiz { quiz_id, atom_id } => changes.append(
                view,
                &event_log::quiz_presented(p.id.clone(), atom_id.clone(), quiz_id.clone()),
            )?,
            Action::PresentLesson { atom_id } => {
                changes.append(
                    view,
                    &event_log::lesson_taught(p.id.clone(), atom_id.clone()),
                )?;
            }
            _ => {}
        }

        if p.strategy == Strategy::TopDown
            && !subpath.is_empty()
            && subpath.iter().all(|a| is_atom_complete(&g, &progress, a))
        {
            path::set_subpath(changes, &p.id, &[]);
        }
        Ok(envelope)
    })
    .await
}

/// Scheduled action for a learning path.
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
    /// Return the atom ID associated with this action, or `None` for [`Action::Done`].
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

/// Compute the highest-priority action for `p` filtered by `mode`.
pub fn next_action(
    g: &Graph,
    p: &PathFile,
    targets: &[String],
    progress: &PathProgress,
    due_quizzes: &[(String, DateTime<Utc>)],
    subpath: &[String],
    mode: NextMode,
) -> Action {
    if mode != NextMode::New
        && let Some((quiz_id, atom_id)) = first_due_card(g, due_quizzes)
    {
        return Action::PresentQuiz { quiz_id, atom_id };
    }
    if mode == NextMode::Due {
        return Action::Done;
    }

    match p.strategy {
        Strategy::BottomUp => {
            let mut visited = HashSet::new();
            for target in targets {
                if let Some(action) = next_atom_action(g, progress, target, &mut visited) {
                    return action;
                }
            }
            Action::Done
        }
        Strategy::TopDown => {
            for atom in subpath.iter().chain(targets) {
                if let Some(action) = atom_action(g, progress, atom) {
                    return action;
                }
            }
            Action::Done
        }
    }
}

fn next_atom_action(
    g: &Graph,
    progress: &PathProgress,
    id: &str,
    visited: &mut HashSet<String>,
) -> Option<Action> {
    if !visited.insert(id.to_string()) {
        return None;
    }
    let c = g.by_id.get(id)?;

    for prereq in &c.prerequisites {
        if let Some(action) = next_atom_action(g, progress, prereq, visited) {
            return Some(action);
        }
    }

    if !c.children_ids.is_empty() {
        for child_id in &c.children_ids {
            if let Some(action) = next_atom_action(g, progress, child_id, visited) {
                return Some(action);
            }
        }
        return None;
    }

    atom_action(g, progress, id)
}

fn atom_action(g: &Graph, progress: &PathProgress, id: &str) -> Option<Action> {
    let c = g.by_id.get(id)?;
    if c.lesson.is_none() {
        return Some(Action::CreateLesson {
            atom_id: id.to_string(),
        });
    }
    if !progress.lesson_taught(id) {
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
                if !progress.quiz_answered_correctly(&quiz.id) {
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

/// Return `true` if `events` contains a `LessonTaught` or `LessonAuthored` event for `atom_id`.
pub fn lesson_taught_in_path(events: &[Event], atom_id: &str) -> bool {
    events.iter().any(|e| {
        matches!(e.kind, EventKind::LessonTaught | EventKind::LessonAuthored)
            && e.atom.as_deref() == Some(atom_id)
    })
}

fn first_due_card(g: &Graph, due_quizzes: &[(String, DateTime<Utc>)]) -> Option<(String, String)> {
    for (quiz_id, _) in due_quizzes {
        let atom_id = atom_from_quiz_id(quiz_id)?;
        let atom = g.by_id.get(&atom_id)?;
        if atom.quizzes.iter().any(|q| &q.id == quiz_id) {
            return Some((quiz_id.clone(), atom_id));
        }
    }
    None
}

/// Return `true` if `atom_id` has a stored lesson and passed quizzes for all 3 difficulties.
pub fn is_atom_complete(g: &Graph, progress: &PathProgress, atom_id: &str) -> bool {
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
            .is_some_and(|q| progress.quiz_answered_correctly(&q.id))
    })
}

/// Presentation and answer history for a quiz card.
#[derive(Serialize, Default, Debug)]
pub struct QuizHistory {
    pub repetitions: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_presented_at: Option<DateTime<Utc>>,
    pub correct_count: u32,
    pub total_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correct_pct: Option<u32>,
    pub recent_ratings: Vec<Rating>,
}

/// Compute presentation and answer statistics for `quiz_id` in `ps`.
pub fn quiz_history(ps: &PathState, quiz_id: &str) -> QuizHistory {
    let Some(c) = ps.cards.get(quiz_id) else {
        return QuizHistory::default();
    };
    let correct = c.reps - c.lapses;
    QuizHistory {
        repetitions: c.presented,
        last_presented_at: c.last_presented_at,
        correct_count: correct,
        total_count: c.reps,
        correct_pct: percent(correct, c.reps),
        recent_ratings: c.recent.clone(),
    }
}

fn percent(numerator: u32, denominator: u32) -> Option<u32> {
    if denominator == 0 {
        return None;
    }
    let num = u64::from(numerator) * 100 + u64::from(denominator) / 2;
    Some(u32::try_from(num / u64::from(denominator)).expect("percent fits in u32"))
}

/// Compute lesson presentation statistics for `atom_id` in `ps`.
pub fn lesson_history(ps: &PathState, atom_id: &str) -> LessonHistory {
    ps.taught
        .get(atom_id)
        .map_or_else(LessonHistory::default, |t| LessonHistory {
            repetitions: t.count,
            last_presented_at: t.last_at,
        })
}

/// Output envelope returned by [`compute_next`].
#[derive(Serialize)]
pub struct Envelope {
    schema_version: u32,
    action: String,
    path: String,
    payload: Payload,
    #[serde(skip)]
    next: Action,
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

#[derive(Serialize, Default, Debug)]
pub struct LessonHistory {
    pub repetitions: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_presented_at: Option<DateTime<Utc>>,
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
    /// Return the scheduled [`Action`] for this envelope.
    pub fn action(&self) -> &Action {
        &self.next
    }

    #[allow(clippy::too_many_lines)]
    fn build(ps: &PathState, g: &Graph, action: Action) -> Self {
        let p = &ps.file;
        let next = action.clone();
        match action {
            Action::PresentLesson { atom_id } => {
                let c = g.by_id.get(&atom_id).expect("atom exists in graph");
                let atom = AtomBrief {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    description: c.description.clone(),
                    lesson: c.lesson.clone(),
                };
                let history = lesson_history(ps, &atom_id);
                Envelope {
                    next,
                    schema_version: 1,
                    action: "present_lesson".to_string(),
                    path: p.id.clone(),
                    payload: Payload::PresentLesson(PresentLessonPayload {
                        atom,
                        reason: "not_taught",
                        history,
                        next_step: "mt path next".to_string(),
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
                let history = quiz_history(ps, &quiz_id);
                Envelope {
                    next,
                    schema_version: 1,
                    action: "present_quiz".to_string(),
                    path: p.id.clone(),
                    payload: Payload::PresentQuiz(PresentQuizPayload {
                        atom,
                        quiz,
                        history,
                        next_step: format!(
                            "mt quiz answer {quiz_id} --rating {{again|hard|good|easy}}"
                        ),
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
                    next,
                    schema_version: 1,
                    action: "create_lesson".to_string(),
                    path: p.id.clone(),
                    payload: Payload::CreateLesson(CreateLessonPayload {
                        atom,
                        prerequisites,
                        next_step: format!("mt lesson upsert {atom_id} --body TEXT"),
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
                    "mt quiz create {atom_id} --difficulty {difficulty} \
                     --question TEXT --answer TEXT [--rubric TEXT]"
                );
                Envelope {
                    next,
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
                next,
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
            })
        })
        .collect()
}
