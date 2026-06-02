//! `mt path next` scheduler: action selection + AYML envelope output.

use std::collections::HashSet;
use std::path::Path;

use chrono::{DateTime, Utc};
use libsql::Connection;
use serde::Serialize;

use crate::answer::atom_from_quiz_id;
use crate::cards;
use crate::db;
use crate::event_log;
use crate::graph::{FlatConcept, Graph};
use crate::path::{self, PathFile};
use crate::progress::PathProgress;
use crate::types::{Difficulty, QuizType, Rating};
use crate::{Error, Result};

const DIFFICULTIES: [Difficulty; 3] = [Difficulty::Easy, Difficulty::Medium, Difficulty::Hard];

pub async fn cmd_path_next(
    conn: &Connection,
    path_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<()> {
    let envelope = compute_next(conn, path_id, graph_dir).await?;
    let text = ayml::to_string(&envelope).map_err(|e| Error::AymlSerialize(e.to_string()))?;
    print!("{text}");
    Ok(())
}

/// Pick the next action for `path_id` and auto-log its
/// `quiz_presented` / `lesson_taught` side effect.
pub async fn compute_next(
    conn: &Connection,
    path_id: Option<&str>,
    graph_dir: Option<&Path>,
) -> Result<Envelope> {
    let tx = conn.transaction().await?;
    let id = path::resolve_id(&tx, path_id).await?;
    let g = Graph::load_for_path(&tx, graph_dir).await?;
    let p = path::load_path(&tx, &id).await?;
    let progress = PathProgress::load(&tx, &id).await?;
    let due = cards::due_quizzes(&tx, &id, Utc::now()).await?;

    let action = next_action(&g, &p, &progress, &due);
    // Build the envelope first — its history aggregates count past
    // presentations only, not the `quiz_presented` / `lesson_taught`
    // we are about to log below.
    let envelope = Envelope::build(&tx, &g, &p, action.clone()).await?;

    match &action {
        Action::PresentQuiz { quiz_id, atom_id } => {
            event_log::append(
                &tx,
                &event_log::quiz_presented(p.id.clone(), atom_id.clone(), quiz_id.clone()),
            )
            .await?;
        }
        Action::PresentLesson { atom_id } => {
            event_log::append(
                &tx,
                &event_log::lesson_taught(p.id.clone(), atom_id.clone()),
            )
            .await?;
        }
        _ => {}
    }
    tx.commit().await?;

    Ok(envelope)
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
/// An atom isn't considered complete — and the walker doesn't advance
/// past it — until its lesson is stored, all three difficulty slots are
/// filled, and each quiz has at least one non-`Again` answer.
pub fn next_action(
    g: &Graph,
    p: &PathFile,
    progress: &PathProgress,
    due_quizzes: &[(String, DateTime<Utc>)],
) -> Action {
    if let Some((quiz_id, atom_id)) = first_due_card(g, due_quizzes) {
        return Action::PresentQuiz { quiz_id, atom_id };
    }

    let mut visited = HashSet::new();
    for target in &p.target_atoms {
        if let Some(action) = next_atom_action(g, progress, target, &mut visited) {
            return action;
        }
    }
    Action::Done
}

/// Walk an atom (and its prereqs / children) and return the first
/// pending action, or `None` if everything reachable is complete.
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

/// First quiz that's both due and still present in the merged graph.
/// Tombstoned or graph-pruned quizzes are silently skipped.
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

/// "Complete" = lesson stored in the merged graph AND all three
/// difficulty quizzes exist AND each has at least one correct answer
/// (per `PathProgress`).
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

// ── Quiz / lesson history (targeted SQL — one quiz_id or atom_id) ──

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

/// Past presentations, answer counts, and the last 10 ratings for one
/// quiz card on one path.
pub async fn compute_quiz_history(
    conn: &Connection,
    path_id: &str,
    quiz_id: &str,
) -> Result<QuizHistory> {
    let mut h = QuizHistory::default();

    // Presentations: count and most-recent ts (one row, aggregate).
    let mut rows = conn
        .query(
            "SELECT COUNT(*), MAX(ts) FROM events \
             WHERE path_id = ? AND kind = 'quiz_presented' AND quiz_id = ?",
            libsql::params![path_id, quiz_id],
        )
        .await?;
    if let Some(row) = rows.next().await? {
        let count: i64 = row.get(0)?;
        h.repetitions = u32::try_from(count)
            .map_err(|_| Error::CardsCorrupt(format!("bad presented count {count}")))?;
        let max_ts: Option<String> = row.get(1)?;
        h.last_presented_at = max_ts.as_deref().map(db::parse_ts).transpose()?;
    }

    // `lapses <= reps` is an invariant of `apply_answer_to_cache`;
    // checked_sub surfaces a broken cache instead of silently zeroing.
    if let Some(card) = cards::read_card(conn, path_id, quiz_id).await? {
        h.total_count = card.reps;
        h.correct_count = card.reps.checked_sub(card.lapses).ok_or_else(|| {
            Error::CardsCorrupt(format!(
                "lapses {} > reps {} for {path_id}/{quiz_id}",
                card.lapses, card.reps
            ))
        })?;
        h.correct_pct = percent(h.correct_count, h.total_count);
    }

    // Recent ratings: newest first, capped at 10. `quiz_answered`
    // events always carry a rating (validated by `event_log::append`).
    let mut rows = conn
        .query(
            "SELECT rating FROM events \
             WHERE path_id = ? AND kind = 'quiz_answered' AND quiz_id = ? \
             ORDER BY id DESC LIMIT 10",
            libsql::params![path_id, quiz_id],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let r: i64 = row.get(0)?;
        h.recent_ratings.push(Rating::try_from(r)?);
    }

    Ok(h)
}

/// Nearest-integer percent, or `None` when `denominator == 0`.
fn percent(numerator: u32, denominator: u32) -> Option<u32> {
    if denominator == 0 {
        return None;
    }
    let num = u64::from(numerator) * 100 + u64::from(denominator) / 2;
    Some(u32::try_from(num / u64::from(denominator)).expect("percent fits in u32"))
}

pub async fn compute_lesson_history(
    conn: &Connection,
    path_id: &str,
    atom_id: &str,
) -> Result<LessonHistory> {
    let mut h = LessonHistory::default();
    let mut rows = conn
        .query(
            "SELECT COUNT(*), MAX(ts) FROM events \
             WHERE path_id = ? AND kind = 'lesson_taught' AND atom_id = ?",
            libsql::params![path_id, atom_id],
        )
        .await?;
    if let Some(row) = rows.next().await? {
        let count: i64 = row.get(0)?;
        h.repetitions = u32::try_from(count)
            .map_err(|_| Error::CardsCorrupt(format!("bad lesson_taught count {count}")))?;
        let max_ts: Option<String> = row.get(1)?;
        h.last_presented_at = max_ts.as_deref().map(db::parse_ts).transpose()?;
    }
    Ok(h)
}

// ── AYML output shape ──────────────────────────────────────────────

#[derive(Serialize)]
pub struct Envelope {
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
    async fn build(conn: &Connection, g: &Graph, p: &PathFile, action: Action) -> Result<Self> {
        match action {
            Action::PresentLesson { atom_id } => {
                let c = g.by_id.get(&atom_id).expect("atom exists in graph");
                let atom = AtomBrief {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    description: c.description.clone(),
                    lesson: c.lesson.clone(),
                };
                let history = compute_lesson_history(conn, &p.id, &atom_id).await?;
                Ok(Envelope {
                    schema_version: 1,
                    action: "present_lesson".to_string(),
                    path: p.id.clone(),
                    payload: Payload::PresentLesson(PresentLessonPayload {
                        atom,
                        reason: "not_taught",
                        history,
                        next_step: "mt path next".to_string(),
                    }),
                })
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
                let history = compute_quiz_history(conn, &p.id, &quiz_id).await?;
                Ok(Envelope {
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
                })
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
                Ok(Envelope {
                    schema_version: 1,
                    action: "create_lesson".to_string(),
                    path: p.id.clone(),
                    payload: Payload::CreateLesson(CreateLessonPayload {
                        atom,
                        prerequisites,
                        next_step: format!("mt lesson upsert {atom_id} --body TEXT"),
                    }),
                })
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
                Ok(Envelope {
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
                })
            }
            Action::Done => Ok(Envelope {
                schema_version: 1,
                action: "done".to_string(),
                path: p.id.clone(),
                payload: Payload::Done(DonePayload {
                    message: "Path complete.".into(),
                }),
            }),
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
