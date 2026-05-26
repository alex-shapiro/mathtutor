//! Curriculum overlay: lessons and quizzes a user has authored on top of
//! the shipped curriculum.
//!
//! Stored in the SQL tables `overlay_lessons`, `overlay_quizzes`, and
//! `overlay_removed_quizzes`. The shipped curriculum is read-only,
//! compiled into the binary. User-authored content lives in these
//! tables and is merged on top of the canonical graph by
//! `Graph::load_for_path`.

use std::collections::{BTreeMap, BTreeSet};

use libsql::{Connection, Row, params};
use serde::Serialize;

use crate::graph::{Quiz, QuizRaw};
use crate::types::{Difficulty, QuizType};
use crate::{Error, Result};

/// In-memory snapshot of the global overlay. Produced by [`load`] and
/// consumed by `Graph::apply_overlay`; also the wire shape printed by
/// `mt overlay dump`.
#[derive(Debug, Default, Serialize)]
pub struct Overlay {
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub atoms: BTreeMap<String, OverlayAtom>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct OverlayAtom {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lesson: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub quizzes: Vec<QuizRaw>,
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub removed: BTreeSet<String>,
}

impl OverlayAtom {
    pub fn quizzes_flat(&self) -> Vec<Quiz> {
        self.quizzes.iter().cloned().map(Quiz::from).collect()
    }
}

// ── SQL load ────────────────────────────────────────────────────────

/// Read the global overlay into an in-memory `Overlay`, keyed by atom.
/// Returns an empty overlay if no rows are present.
pub async fn load(conn: &Connection) -> Result<Overlay> {
    let mut atoms: BTreeMap<String, OverlayAtom> = BTreeMap::new();

    let mut rows = conn
        .query("SELECT atom_id, body FROM overlay_lessons", params![])
        .await?;
    while let Some(row) = rows.next().await? {
        let atom_id: String = row.get(0)?;
        let body: String = row.get(1)?;
        atoms.entry(atom_id).or_default().lesson = Some(body);
    }

    let mut rows = conn
        .query(
            "SELECT atom_id, quiz_id, difficulty, kind, question, answer, rubric \
             FROM overlay_quizzes ORDER BY atom_id, quiz_id",
            params![],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let (atom_id, quiz) = row_to_overlay_quiz(&row)?;
        atoms.entry(atom_id).or_default().quizzes.push(quiz);
    }

    // Tombstones may target a quiz that hasn't been merged yet (e.g.,
    // an authored quiz the user later removed) — re-derive the atom
    // from the quiz id so the tombstone always lands on the right atom.
    let mut rows = conn
        .query(
            "SELECT q.atom_id, r.quiz_id \
             FROM overlay_removed_quizzes r \
             LEFT JOIN overlay_quizzes q ON q.quiz_id = r.quiz_id",
            params![],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let atom_from_overlay: Option<String> = row.get(0)?;
        let quiz_id: String = row.get(1)?;
        let atom_id = atom_from_overlay
            .or_else(|| crate::answer::atom_from_quiz_id(&quiz_id))
            .unwrap_or_else(|| quiz_id.clone());
        atoms.entry(atom_id).or_default().removed.insert(quiz_id);
    }

    Ok(Overlay { atoms })
}

fn row_to_overlay_quiz(row: &Row) -> Result<(String, QuizRaw)> {
    let atom_id: String = row.get(0)?;
    let quiz_id: String = row.get(1)?;
    let difficulty_str: String = row.get(2)?;
    let kind_str: Option<String> = row.get(3)?;
    let question: String = row.get(4)?;
    let answer: String = row.get(5)?;
    let rubric: Option<String> = row.get(6)?;

    let difficulty = parse_difficulty(&difficulty_str)?;
    let kind = kind_str.as_deref().map(parse_kind).transpose()?;
    Ok((
        atom_id,
        QuizRaw {
            id: quiz_id,
            difficulty,
            kind,
            question,
            answer,
            rubric,
        },
    ))
}

fn parse_difficulty(s: &str) -> Result<Difficulty> {
    match s {
        "easy" => Ok(Difficulty::Easy),
        "medium" => Ok(Difficulty::Medium),
        "hard" => Ok(Difficulty::Hard),
        other => Err(Error::AymlParse {
            path: std::path::PathBuf::from("<overlay_quizzes.difficulty>"),
            message: format!("unknown difficulty: {other}"),
        }),
    }
}

fn parse_kind(s: &str) -> Result<QuizType> {
    match s {
        "free_text" => Ok(QuizType::FreeText),
        "multiple_choice" => Ok(QuizType::MultipleChoice),
        other => Err(Error::AymlParse {
            path: std::path::PathBuf::from("<overlay_quizzes.kind>"),
            message: format!("unknown quiz kind: {other}"),
        }),
    }
}

// ── SQL mutators ────────────────────────────────────────────────────

/// Insert a lesson for `atom_id`. Caller is responsible for ensuring no
/// shipped/overlay lesson already exists (typically by consulting the
/// merged graph) — a row collision returns `LessonAlreadyExists`.
pub async fn add_lesson(conn: &Connection, atom_id: &str, body: &str) -> Result<()> {
    let res = conn
        .execute(
            "INSERT INTO overlay_lessons(atom_id, body) VALUES (?, ?)",
            params![atom_id.to_string(), body.to_string()],
        )
        .await;
    match res {
        Ok(_) => Ok(()),
        Err(e) if is_unique_violation(&e) => Err(Error::LessonAlreadyExists(atom_id.to_string())),
        Err(e) => Err(e.into()),
    }
}

/// Insert a new quiz row. Caller supplies a fresh, globally-unique
/// quiz id (typically derived from the highest existing id in the
/// merged view via `next_quiz_id`).
pub async fn add_quiz(conn: &Connection, atom_id: &str, quiz: &QuizRaw) -> Result<()> {
    conn.execute(
        "INSERT INTO overlay_quizzes(atom_id, quiz_id, difficulty, kind, question, answer, rubric) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        params![
            atom_id.to_string(),
            quiz.id.clone(),
            quiz.difficulty.to_string(),
            quiz.kind.map(quiz_kind_to_str),
            quiz.question.clone(),
            quiz.answer.clone(),
            quiz.rubric.clone(),
        ],
    )
    .await?;
    Ok(())
}

/// Upsert an amended quiz into the overlay. `base` is the quiz's
/// current state in the merged view (shipped + overlay); only fields
/// supplied as `Some` change. Un-tombstones the quiz if it was
/// previously removed.
#[allow(clippy::too_many_arguments)]
pub async fn amend_quiz(
    conn: &Connection,
    atom_id: &str,
    base: &QuizRaw,
    new_difficulty: Option<Difficulty>,
    new_question: Option<String>,
    new_answer: Option<String>,
    new_rubric: Option<String>,
    new_type: Option<QuizType>,
) -> Result<()> {
    let updated = QuizRaw {
        id: base.id.clone(),
        difficulty: new_difficulty.unwrap_or(base.difficulty),
        kind: match new_type {
            Some(t) => (t != QuizType::FreeText).then_some(t),
            None => base.kind,
        },
        question: new_question.unwrap_or_else(|| base.question.clone()),
        answer: new_answer.unwrap_or_else(|| base.answer.clone()),
        rubric: new_rubric.or_else(|| base.rubric.clone()),
    };

    conn.execute(
        "INSERT INTO overlay_quizzes(atom_id, quiz_id, difficulty, kind, question, answer, rubric) \
         VALUES (?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(quiz_id) DO UPDATE SET \
            atom_id    = excluded.atom_id, \
            difficulty = excluded.difficulty, \
            kind       = excluded.kind, \
            question   = excluded.question, \
            answer     = excluded.answer, \
            rubric     = excluded.rubric",
        params![
            atom_id.to_string(),
            updated.id.clone(),
            updated.difficulty.to_string(),
            updated.kind.map(quiz_kind_to_str),
            updated.question.clone(),
            updated.answer.clone(),
            updated.rubric.clone(),
        ],
    )
    .await?;
    conn.execute(
        "DELETE FROM overlay_removed_quizzes WHERE quiz_id = ?",
        params![updated.id.clone()],
    )
    .await?;
    Ok(())
}

/// Tombstone `quiz_id` so it stops appearing in the merged view.
/// Idempotent.
pub async fn remove_quiz(conn: &Connection, quiz_id: &str) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO overlay_removed_quizzes(quiz_id) VALUES (?)",
        params![quiz_id.to_string()],
    )
    .await?;
    Ok(())
}

fn quiz_kind_to_str(k: QuizType) -> String {
    match k {
        QuizType::FreeText => "free_text".to_string(),
        QuizType::MultipleChoice => "multiple_choice".to_string(),
    }
}

/// libSQL surfaces `SQLite` unique-constraint failures as `SqliteFailure`
/// with code 2067 (`SQLITE_CONSTRAINT_UNIQUE`) — match on the rendered
/// message to detect them without depending on a private error variant.
fn is_unique_violation(e: &libsql::Error) -> bool {
    let msg = e.to_string();
    msg.contains("UNIQUE constraint failed") || msg.contains("PRIMARY KEY")
}

// ── `mt overlay dump` ──────────────────────────────────────────────

pub async fn cmd_dump(conn: &Connection) -> Result<()> {
    let overlay = load(conn).await?;
    let text = ayml::to_string(&overlay).map_err(|e| Error::AymlSerialize(e.to_string()))?;
    print!("{text}");
    Ok(())
}
