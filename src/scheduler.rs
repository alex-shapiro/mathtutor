//! Action scheduler and AYML output envelope for `mt path next`.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::db::Db;
use crate::event_log::{Event, EventKind};
use crate::graph::{self, FlatConcept, Graph};
use crate::path::PathFile;
use crate::progress::{PathProgress, QuizSlot};
use crate::subpath;
use crate::surface::{Field, Step, Surface, Tool, Value};
use crate::types::{Difficulty, QuizType, Rating, Strategy};
use crate::view::{PathState, View, Want};
use crate::{Error, Result};

/// Compute the next action for `path_id` and print its AYML envelope for `mt path next`.
pub async fn cmd_path_next(db: &Db, path_id: Option<&str>, mode: NextMode) -> Result<()> {
    crate::print_ayml(&compute_next(db, path_id, mode, Surface::Cli).await?)
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

/// Inputs that every view of a path reads: the merged graph, the progress, the target atoms, the subpath, and the due cards.
pub struct PathPlan<'v> {
    pub state: &'v PathState,
    pub graph: Arc<Graph>,
    pub progress: PathProgress,
    /// Topologically sorted target atoms.
    pub targets: Vec<String>,
    /// Atoms of the stored subpath.
    pub subpath: Vec<String>,
    /// Cards due now, from earliest to latest.
    pub due: Vec<(String, DateTime<Utc>)>,
}

impl<'v> PathPlan<'v> {
    /// Return the plan of the path that `view` selected.
    pub fn of(view: &'v View) -> Result<Self> {
        Self::new(view.path()?, view.graph()?, Utc::now())
    }

    /// Return the plan of path `state` over `graph` at time `now`.
    pub fn new(state: &'v PathState, graph: Arc<Graph>, now: DateTime<Utc>) -> Result<Self> {
        Ok(Self {
            progress: state.progress(),
            targets: state.file.resolve_targets(&graph)?,
            subpath: subpath::resolved(&state.file, &graph)?,
            due: state.due(now),
            graph,
            state,
        })
    }

    /// Return the highest-priority action of this path in `mode`.
    pub fn next_action(&self, mode: NextMode) -> Action {
        if mode != NextMode::New
            && let Some((quiz_id, atom_id)) = first_due_card(&self.graph, &self.due)
        {
            return Action::PresentQuiz { quiz_id, atom_id };
        }
        if mode == NextMode::Due {
            return Action::NoReviewsDue;
        }
        self.teaching_order()
            .iter()
            .find_map(|atom| atom_action(self.graph.by_id.get(atom)?, &self.progress))
            .unwrap_or(Action::Done)
    }

    /// Return the atoms of this path with untaught lessons, in teaching order.
    pub fn upcoming(&self) -> Vec<String> {
        let mut order = self.teaching_order();
        order.retain(|atom| !self.progress.lesson_taught(atom));
        order
    }

    /// Return `true` if the atom `atom_id` is complete on this path.
    pub fn is_complete(&self, atom_id: &str) -> bool {
        is_atom_complete(&self.graph, &self.progress, atom_id)
    }

    /// Return the atoms of this path in the order that the scheduler teaches them.
    ///
    /// `bottom_up` visits the prerequisites and the children of each target before the target. `top_down` lists the subpath and then the targets.
    fn teaching_order(&self) -> Vec<String> {
        let g = &self.graph;
        let mut visited = HashSet::new();
        let mut out = Vec::new();
        match self.state.file.strategy {
            Strategy::BottomUp => {
                for target in &self.targets {
                    visit_bottom_up(g, target, &mut visited, &mut out);
                }
            }
            Strategy::TopDown => {
                for id in self.subpath.iter().chain(&self.targets) {
                    let is_atom = g.by_id.get(id).is_some_and(|c| c.children_ids.is_empty());
                    if is_atom && visited.insert(id.clone()) {
                        out.push(id.clone());
                    }
                }
            }
        }
        out
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
        let plan = PathPlan::of(view)?;
        let p = &plan.state.file;
        let action = plan.next_action(mode);
        // Build the envelope before appending presentation events so history reflects prior attempts.
        let envelope = Envelope::build(plan.state, &plan.graph, action.clone(), surface);

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
            && !plan.subpath.is_empty()
            && plan.subpath.iter().all(|a| plan.is_complete(a))
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
    /// Return the name of this action in the `action` field of an envelope.
    pub fn name(&self) -> &'static str {
        match self {
            Action::PresentQuiz { .. } => "present_quiz",
            Action::CreateLesson { .. } => "create_lesson",
            Action::PresentLesson { .. } => "present_lesson",
            Action::CreateQuiz { .. } => "create_quiz",
            Action::NoReviewsDue => "no_reviews_due",
            Action::Done => "done",
        }
    }

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

fn visit_bottom_up(g: &Graph, id: &str, visited: &mut HashSet<String>, out: &mut Vec<String>) {
    if !visited.insert(id.to_string()) {
        return;
    }
    let Some(c) = g.by_id.get(id) else { return };
    for prereq in &c.prerequisites {
        visit_bottom_up(g, prereq, visited, out);
    }
    if c.children_ids.is_empty() {
        out.push(id.to_string());
    }
    for child in &c.children_ids {
        visit_bottom_up(g, child, visited, out);
    }
}

/// Return the next action that atom `c` needs on a path with `progress`, or `None` if the atom is complete.
pub fn atom_action(c: &FlatConcept, progress: &PathProgress) -> Option<Action> {
    let atom_id = c.id.clone();
    if c.lesson.is_none() {
        return Some(Action::CreateLesson { atom_id });
    }
    if !progress.lesson_taught(&c.id) {
        return Some(Action::PresentLesson { atom_id });
    }
    Difficulty::ALL
        .iter()
        .find_map(|&difficulty| match progress.quiz_slot(c, difficulty) {
            QuizSlot::Missing => Some(Action::CreateQuiz {
                atom_id: atom_id.clone(),
                difficulty,
            }),
            QuizSlot::Authored => {
                c.quizzes
                    .iter()
                    .find(|q| q.difficulty == difficulty)
                    .map(|quiz| Action::PresentQuiz {
                        quiz_id: quiz.id.clone(),
                        atom_id: atom_id.clone(),
                    })
            }
            QuizSlot::Correct => None,
        })
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

/// Return `true` if `atom_id` is in `g` and needs no further action on a path with `progress`.
pub fn is_atom_complete(g: &Graph, progress: &PathProgress, atom_id: &str) -> bool {
    g.by_id
        .get(atom_id)
        .is_some_and(|c| atom_action(c, progress).is_none())
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
    let Some(c) = ps.projection.cards.get(quiz_id) else {
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
    ps.projection
        .taught
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
    #[serde(serialize_with = "serialize_action_name")]
    action: Action,
    path: String,
    /// Command or tool call that performs the action.
    #[serde(skip_serializing_if = "Option::is_none")]
    next_step: Option<String>,
    payload: Payload,
}

fn serialize_action_name<S: serde::Serializer>(
    action: &Action,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_str(action.name())
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
    message: &'static str,
}

impl AtomBrief {
    fn of(c: &FlatConcept) -> Self {
        Self {
            id: c.id.clone(),
            name: c.name.clone(),
            description: c.description.clone(),
            lesson: c.lesson.clone(),
        }
    }
}

impl Envelope {
    /// Return the scheduled [`Action`] for this envelope.
    pub fn action(&self) -> &Action {
        &self.action
    }

    fn build(ps: &PathState, g: &Graph, action: Action, surface: Surface) -> Self {
        let concept = |id: &str| g.by_id.get(id).expect("atom exists in graph");
        let payload = match &action {
            Action::PresentLesson { atom_id } => Payload::PresentLesson(PresentLessonPayload {
                atom: AtomBrief::of(concept(atom_id)),
                reason: "not_taught",
                history: lesson_history(ps, atom_id),
            }),
            Action::PresentQuiz { quiz_id, atom_id } => {
                let c = concept(atom_id);
                let q = c
                    .quizzes
                    .iter()
                    .find(|x| x.id == *quiz_id)
                    .expect("quiz exists in graph");
                Payload::PresentQuiz(PresentQuizPayload {
                    atom: AtomBrief::of(c),
                    quiz: QuizFull {
                        id: q.id.clone(),
                        difficulty: q.difficulty,
                        kind: q.kind.unwrap_or_default(),
                        question: q.question.clone(),
                        answer: q.answer.clone(),
                        rubric: q.rubric.clone(),
                    },
                    history: quiz_history(ps, quiz_id),
                })
            }
            Action::CreateLesson { atom_id } => {
                let c = concept(atom_id);
                Payload::CreateLesson(CreateLessonPayload {
                    atom: AtomBrief::of(c),
                    prerequisites: collect_prereqs(g, c),
                })
            }
            Action::CreateQuiz {
                atom_id,
                difficulty,
            } => {
                let c = concept(atom_id);
                let existing_quizzes = c
                    .quizzes
                    .iter()
                    .map(|q| ExistingQuizBrief {
                        id: q.id.clone(),
                        difficulty: q.difficulty,
                        question: q.question.clone(),
                    })
                    .collect();
                Payload::CreateQuiz(CreateQuizPayload {
                    atom: AtomBrief::of(c),
                    target_difficulty: *difficulty,
                    existing_quizzes,
                    prerequisites: collect_prereqs(g, c),
                })
            }
            Action::NoReviewsDue => Payload::Message(MessagePayload {
                message: "Zero reviews are due.",
            }),
            Action::Done => Payload::Message(MessagePayload {
                message: "Path complete.",
            }),
        };
        Envelope {
            schema_version: 1,
            next_step: next_step(&action, &ps.file.id).map(|s| s.render(surface)),
            path: ps.file.id.clone(),
            action,
            payload,
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
            .arg(Field::Rating, Value::Choice(Rating::SPELLINGS))
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
