//! Action scheduler and AYML output envelope for `mt path next`.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::db::Db;
use crate::event_log::{Event, EventKind};
use crate::graph::{self, FlatConcept, Graph};
use crate::path::PathFile;
use crate::progress::PathProgress;
use crate::subpath;
use crate::surface::{self, Field, Step, Surface, Tool, Value};
use crate::types::{Difficulty, QuizType, Rating, Strategy};
use crate::view::{PathState, Want};
use crate::{Error, Result};

const DIFFICULTIES: [Difficulty; 3] = [Difficulty::Easy, Difficulty::Medium, Difficulty::Hard];

/// Compute the next action for `path_id` and print its AYML envelope for `mt path next`.
pub async fn cmd_path_next(db: &Db, path_id: Option<&str>, mode: NextMode) -> Result<()> {
    let envelope = compute_next(db, path_id, mode, Surface::Cli).await?;
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

/// Select the next action for `path_id`, append its presentation event, and return its envelope with the next step in the syntax of `surface`.
pub async fn compute_next(
    db: &Db,
    path_id: Option<&str>,
    mode: NextMode,
    surface: Surface,
) -> Result<Envelope> {
    db.transact(&Want::path(path_id), |view, changes| {
        let ps = view.path()?;
        let p = &ps.file;
        let g = view.graph()?;
        let progress = ps.progress();
        let due = ps.due(Utc::now());
        let subpath = subpath::resolved(p, &g)?;
        let targets = p.resolve_targets(&g)?;

        let action = next_action(&g, p, &targets, &progress, &due, &subpath, mode);
        // Build the envelope before appending presentation events so history reflects prior attempts.
        let envelope = Envelope::build(ps, &g, action.clone(), surface);

        match &action {
            Action::PresentQuiz { quiz_id, atom_id } => {
                let presented = EventKind::QuizPresented {
                    atom: atom_id.clone(),
                    quiz: quiz_id.clone(),
                };
                changes.append(view, &Event::now(&p.id, presented))?;
            }
            Action::PresentLesson { atom_id } => {
                let taught = EventKind::LessonTaught {
                    atom: atom_id.clone(),
                };
                changes.append(view, &Event::now(&p.id, taught))?;
            }
            _ => {}
        }

        if p.strategy == Strategy::TopDown
            && !subpath.is_empty()
            && subpath.iter().all(|a| is_atom_complete(&g, &progress, a))
        {
            changes.replace_path(&PathFile {
                subpath: Vec::new(),
                ..p.clone()
            });
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
    /// Zero quiz cards are due, in [`NextMode::Due`].
    NoReviewsDue,
    Done,
}

impl Action {
    /// Return the atom ID associated with this action, or `None` if the action has no atom.
    pub fn atom_id(&self) -> Option<&str> {
        match self {
            Action::CreateLesson { atom_id }
            | Action::PresentLesson { atom_id }
            | Action::CreateQuiz { atom_id, .. }
            | Action::PresentQuiz { atom_id, .. } => Some(atom_id),
            Action::NoReviewsDue | Action::Done => None,
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
        return Action::NoReviewsDue;
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

fn first_due_card(g: &Graph, due_quizzes: &[(String, DateTime<Utc>)]) -> Option<(String, String)> {
    due_quizzes.iter().find_map(|(quiz_id, _)| {
        atom_of_live_quiz(g, quiz_id).map(|atom_id| (quiz_id.clone(), atom_id))
    })
}

/// Return the atom ID of `quiz_id` if the merged curriculum contains the quiz.
pub fn atom_of_live_quiz(g: &Graph, quiz_id: &str) -> Option<String> {
    let atom_id = graph::atom_from_quiz_id(quiz_id)?;
    let atom = g.by_id.get(atom_id)?;
    atom.quizzes
        .iter()
        .any(|q| q.id == quiz_id)
        .then(|| atom_id.to_string())
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
    /// Command or tool call that performs the action.
    #[serde(skip_serializing_if = "Option::is_none")]
    next_step: Option<String>,
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
    Message(MessagePayload),
}

#[derive(Serialize)]
struct CreateLessonPayload {
    atom: AtomBrief,
    prerequisites: Vec<PrereqBrief>,
}

#[derive(Serialize)]
struct PresentLessonPayload {
    atom: AtomBrief,
    reason: &'static str,
    history: LessonHistory,
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
}

#[derive(Serialize)]
struct PresentQuizPayload {
    atom: AtomBrief,
    quiz: QuizFull,
    history: QuizHistory,
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
struct MessagePayload {
    message: String,
}

impl Envelope {
    /// Return the scheduled [`Action`] for this envelope.
    pub fn action(&self) -> &Action {
        &self.next
    }

    #[allow(clippy::too_many_lines)]
    fn build(ps: &PathState, g: &Graph, action: Action, surface: Surface) -> Self {
        let p = &ps.file;
        let next_step = next_step(&action, &p.id).map(|s| s.render(surface));
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
                    next_step,
                    schema_version: 1,
                    action: "present_lesson".to_string(),
                    path: p.id.clone(),
                    payload: Payload::PresentLesson(PresentLessonPayload {
                        atom,
                        reason: "not_taught",
                        history,
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
                    next_step,
                    schema_version: 1,
                    action: "present_quiz".to_string(),
                    path: p.id.clone(),
                    payload: Payload::PresentQuiz(PresentQuizPayload {
                        atom,
                        quiz,
                        history,
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
                    next_step,
                    schema_version: 1,
                    action: "create_lesson".to_string(),
                    path: p.id.clone(),
                    payload: Payload::CreateLesson(CreateLessonPayload {
                        atom,
                        prerequisites,
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
                Envelope {
                    next,
                    next_step,
                    schema_version: 1,
                    action: "create_quiz".to_string(),
                    path: p.id.clone(),
                    payload: Payload::CreateQuiz(CreateQuizPayload {
                        atom,
                        target_difficulty: difficulty,
                        existing_quizzes,
                        prerequisites,
                    }),
                }
            }
            Action::NoReviewsDue => Envelope {
                next,
                next_step,
                schema_version: 1,
                action: "no_reviews_due".to_string(),
                path: p.id.clone(),
                payload: Payload::Message(MessagePayload {
                    message: "Zero reviews are due.".into(),
                }),
            },
            Action::Done => Envelope {
                next,
                next_step,
                schema_version: 1,
                action: "done".to_string(),
                path: p.id.clone(),
                payload: Payload::Message(MessagePayload {
                    message: "Path complete.".into(),
                }),
            },
        }
    }
}

/// Return the step that performs `action` on path `path_id`, or `None` for [`Action::Done`].
pub fn next_step(action: &Action, path_id: &str) -> Option<Step> {
    let path = Value::known(path_id);
    let step = match action {
        Action::PresentLesson { .. } | Action::NoReviewsDue => Step::new(Tool::GetNext, path),
        Action::PresentQuiz { quiz_id, .. } => Step::new(Tool::AnswerQuiz, path)
            .arg(Field::QuizId, Value::known(quiz_id))
            .arg(Field::Rating, Value::Choice(surface::RATINGS))
            .arg(Field::UserAnswer, Value::Text("verbatim reply")),
        Action::CreateLesson { atom_id } => Step::new(Tool::UpsertLesson, path)
            .arg(Field::Atom, Value::known(atom_id))
            .arg(Field::Body, Value::Text("lesson")),
        Action::CreateQuiz {
            atom_id,
            difficulty,
        } => Step::new(Tool::CreateQuiz, path)
            .arg(Field::Atom, Value::known(atom_id))
            .arg(Field::Difficulty, Value::known(difficulty.as_str()))
            .arg(Field::Question, Value::Text("question"))
            .arg(Field::Answer, Value::Text("answer"))
            .optional(Field::Rubric, Value::Text("rubric")),
        Action::Done => return None,
    };
    Some(step)
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
