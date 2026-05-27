//! One-shot port of the legacy on-disk AYML state into the libSQL
//! database. Reads `<root>/paths/<id>/{path,log,overlay}.ayml` files
//! produced by versions of `mt` prior to the SQL migration and inserts
//! their contents into `paths`, `path_targets`, `events`, and the three
//! `overlay_*` tables.
//!
//! Idempotency: every write uses `INSERT OR IGNORE`. The presence of a
//! path row is treated as the "already migrated" marker — its `events`
//! and `path_targets` are imported on the first successful insert and
//! skipped on every subsequent run. Overlay rows are PK-keyed (atom or
//! quiz id), so per-row `INSERT OR IGNORE` keeps re-runs safe even when
//! multiple legacy per-path overlays mention the same atom.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use libsql::{Connection, params};
use serde::Deserialize;

use crate::cards;
use crate::db;
use crate::event_log::{Event, EventKind, EventPayload};
use crate::path::mt_home;
use crate::types::{Difficulty, QuizType, Rating};
use crate::{Error, Result};

/// Tally of what landed in SQL during a single migration pass. Used by
/// the CLI to print a human-readable summary, and by tests to assert on
/// idempotency.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MigrationReport {
    pub paths_imported: u32,
    pub paths_skipped: u32,
    pub events_imported: u32,
    pub overlay_lessons_imported: u32,
    pub overlay_quizzes_imported: u32,
    pub overlay_removed_imported: u32,
}

// ── Legacy on-disk shapes ──────────────────────────────────────────
//
// Mirrored from the pre-SQL `path.rs`, `event_log.rs`, and `overlay.rs`
// just enough to round-trip the files. Unknown fields (e.g. legacy
// `schema_version`) are accepted-and-ignored via `#[serde(default)]`.

#[derive(Debug, Deserialize)]
struct LegacyPathFile {
    id: String,
    goal: String,
    created_at: DateTime<Utc>,
    #[serde(default)]
    target_atoms: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct LegacyEvent {
    ts: DateTime<Utc>,
    #[serde(rename = "type")]
    kind: LegacyEventKind,
    path: String,
    #[serde(default)]
    atom: Option<String>,
    #[serde(default)]
    quiz: Option<String>,
    #[serde(default)]
    payload: LegacyEventPayload,
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum LegacyEventKind {
    PathCreated,
    LessonAuthored,
    LessonAmended,
    LessonTaught,
    QuizAuthored,
    QuizPresented,
    QuizAnswered,
    QuizAmended,
    QuizRemoved,
}

impl From<LegacyEventKind> for EventKind {
    fn from(k: LegacyEventKind) -> Self {
        match k {
            LegacyEventKind::PathCreated => EventKind::PathCreated,
            LegacyEventKind::LessonAuthored => EventKind::LessonAuthored,
            LegacyEventKind::LessonAmended => EventKind::LessonAmended,
            LegacyEventKind::LessonTaught => EventKind::LessonTaught,
            LegacyEventKind::QuizAuthored => EventKind::QuizAuthored,
            LegacyEventKind::QuizPresented => EventKind::QuizPresented,
            LegacyEventKind::QuizAnswered => EventKind::QuizAnswered,
            LegacyEventKind::QuizAmended => EventKind::QuizAmended,
            LegacyEventKind::QuizRemoved => EventKind::QuizRemoved,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
struct LegacyEventPayload {
    #[serde(default)]
    rating: Option<Rating>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    user_answer: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LegacyOverlay {
    #[serde(default)]
    atoms: BTreeMap<String, LegacyOverlayAtom>,
}

#[derive(Debug, Deserialize, Default)]
struct LegacyOverlayAtom {
    #[serde(default)]
    lesson: Option<String>,
    #[serde(default)]
    quizzes: Vec<LegacyQuiz>,
    #[serde(default)]
    removed: BTreeSet<String>,
}

#[derive(Debug, Deserialize)]
struct LegacyQuiz {
    id: String,
    difficulty: Difficulty,
    #[serde(rename = "type", default)]
    kind: Option<QuizType>,
    question: String,
    answer: String,
    #[serde(default)]
    rubric: Option<String>,
}

// ── Public entry points ────────────────────────────────────────────

/// CLI hook: resolve the AYML root, run the migration, print a summary.
pub async fn cmd_migrate(conn: &Connection, from: Option<&Path>) -> Result<()> {
    let root = match from {
        Some(p) => p.to_path_buf(),
        None => mt_home()?,
    };
    let report = migrate(conn, &root).await?;
    eprintln!(
        "migrated {paths} path(s) ({skipped} already present), {events} event(s), \
         overlay: {lessons} lesson(s) / {quizzes} quiz(zes) / {removed} tombstone(s) [from {root}]",
        paths = report.paths_imported,
        skipped = report.paths_skipped,
        events = report.events_imported,
        lessons = report.overlay_lessons_imported,
        quizzes = report.overlay_quizzes_imported,
        removed = report.overlay_removed_imported,
        root = root.display(),
    );
    Ok(())
}

/// Walk `<root>/paths/<id>/` and import each path's AYML state. Safe to
/// re-run: every write goes through `INSERT OR IGNORE`, and per-path
/// events are imported only when the path row itself is freshly inserted
/// so the auto-increment `id` column doesn't accumulate duplicates.
pub async fn migrate(conn: &Connection, root: &Path) -> Result<MigrationReport> {
    let paths_root = root.join("paths");
    let mut report = MigrationReport::default();

    if !paths_root.exists() {
        return Ok(report);
    }
    for dir in list_path_dirs(&paths_root)? {
        migrate_one_path(conn, &dir, &mut report).await?;
    }
    Ok(report)
}

// ── Per-path migration ─────────────────────────────────────────────

async fn migrate_one_path(
    conn: &Connection,
    path_dir: &Path,
    report: &mut MigrationReport,
) -> Result<()> {
    let path_file = path_dir.join("path.ayml");
    let overlay_file = path_dir.join("overlay.ayml");
    let log_file = path_dir.join("log.ayml");

    // A path directory with no `path.ayml` is junk left by a partial
    // `mt new` or by hand; skip it entirely. Overlays can't be merged
    // without knowing which path they belonged to (for ordering only —
    // overlays are global now), but absent a path file we have nothing
    // to anchor on.
    if !path_file.exists() {
        return Ok(());
    }
    let p = read_ayml::<LegacyPathFile>(&path_file)?;

    let inserted = conn
        .execute(
            "INSERT OR IGNORE INTO paths(id, goal, created_at) VALUES (?, ?, ?)",
            params![p.id.as_str(), p.goal.as_str(), db::format_ts(p.created_at)],
        )
        .await?;
    let fresh = inserted == 1;
    if fresh {
        report.paths_imported += 1;
        for (i, atom) in p.target_atoms.iter().enumerate() {
            let position = i64::try_from(i).expect("position fits in i64");
            conn.execute(
                "INSERT OR IGNORE INTO path_targets(path_id, atom_id, position) \
                 VALUES (?, ?, ?)",
                params![p.id.as_str(), atom.as_str(), position],
            )
            .await?;
        }
    } else {
        report.paths_skipped += 1;
    }

    // Events are auto-incremented and have no natural unique key, so we
    // only import them the first time we see this path id. A subsequent
    // run after the user has already used the SQL-backed `mt` would
    // otherwise duplicate every quiz answer.
    if fresh && log_file.exists() {
        let events = read_ayml::<Vec<LegacyEvent>>(&log_file)?;
        for e in events {
            insert_event(conn, &e).await?;
            report.events_imported += 1;
        }
        // Rebuild the cards cache from the freshly-imported log so the
        // scheduler sees correct due dates immediately after migration.
        cards::recompute(conn, &p.id).await?;
    }

    if overlay_file.exists() {
        let overlay = read_ayml::<LegacyOverlay>(&overlay_file)?;
        import_overlay(conn, overlay, report).await?;
    }
    Ok(())
}

async fn insert_event(conn: &Connection, e: &LegacyEvent) -> Result<()> {
    let kind: EventKind = e.kind.into();
    let event = Event {
        ts: e.ts,
        kind,
        path: e.path.clone(),
        atom: e.atom.clone(),
        quiz: e.quiz.clone(),
        payload: EventPayload {
            rating: e.payload.rating,
            reason: e.payload.reason.clone(),
            user_answer: e.payload.user_answer.clone(),
        },
    };
    // Insert directly: `event_log::append` would also fold a
    // `QuizAnswered` into the cards cache row-by-row, which we'd just
    // overwrite with `cards::recompute` at the end. Skipping it keeps
    // the per-event cost constant.
    let stored_reason = event.payload.reason.as_deref();
    let stored_user_answer = event.payload.user_answer.as_deref();
    let payload_json = if stored_reason.is_none() && stored_user_answer.is_none() {
        None
    } else {
        Some(serde_json::to_string(&StoredPayload {
            reason: stored_reason,
            user_answer: stored_user_answer,
        })?)
    };
    let rating_int = event.payload.rating.map(i64::from);
    conn.execute(
        "INSERT INTO events(ts, kind, path_id, atom_id, quiz_id, rating, payload) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        params![
            db::format_ts(event.ts),
            event.kind.as_str(),
            event.path.as_str(),
            event.atom.as_deref(),
            event.quiz.as_deref(),
            rating_int,
            payload_json,
        ],
    )
    .await?;
    Ok(())
}

#[derive(serde::Serialize)]
struct StoredPayload<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_answer: Option<&'a str>,
}

async fn import_overlay(
    conn: &Connection,
    overlay: LegacyOverlay,
    report: &mut MigrationReport,
) -> Result<()> {
    for (atom_id, entry) in overlay.atoms {
        if let Some(body) = entry.lesson {
            let n = conn
                .execute(
                    "INSERT OR IGNORE INTO overlay_lessons(atom_id, body) VALUES (?, ?)",
                    params![atom_id.as_str(), body.as_str()],
                )
                .await?;
            report.overlay_lessons_imported += u32::try_from(n).unwrap_or(0);
        }
        for q in entry.quizzes {
            let n = conn
                .execute(
                    "INSERT OR IGNORE INTO overlay_quizzes\
                     (atom_id, quiz_id, difficulty, kind, question, answer, rubric) \
                     VALUES (?, ?, ?, ?, ?, ?, ?)",
                    params![
                        atom_id.as_str(),
                        q.id.as_str(),
                        q.difficulty.as_str(),
                        q.kind.map(QuizType::as_str),
                        q.question.as_str(),
                        q.answer.as_str(),
                        q.rubric.as_deref(),
                    ],
                )
                .await?;
            report.overlay_quizzes_imported += u32::try_from(n).unwrap_or(0);
        }
        for quiz_id in entry.removed {
            let n = conn
                .execute(
                    "INSERT OR IGNORE INTO overlay_removed_quizzes(quiz_id) VALUES (?)",
                    params![quiz_id.as_str()],
                )
                .await?;
            report.overlay_removed_imported += u32::try_from(n).unwrap_or(0);
        }
    }
    Ok(())
}

// ── Filesystem helpers ─────────────────────────────────────────────

fn list_path_dirs(paths_root: &Path) -> Result<Vec<PathBuf>> {
    let entries = fs::read_dir(paths_root).map_err(|e| Error::FileIo {
        path: paths_root.to_path_buf(),
        source: e,
    })?;
    // Sort by directory name so migration order is deterministic — the
    // legacy ids are timestamp-based, so this also recovers chronology.
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(std::result::Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    dirs.sort();
    Ok(dirs)
}

fn read_ayml<T: for<'de> Deserialize<'de>>(file: &Path) -> Result<T> {
    let f = fs::File::open(file).map_err(|e| Error::FileIo {
        path: file.to_path_buf(),
        source: e,
    })?;
    ayml::from_reader(BufReader::new(f)).map_err(|e| Error::AymlParse {
        path: file.to_path_buf(),
        message: e.to_string(),
    })
}
