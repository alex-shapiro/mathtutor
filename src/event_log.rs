//! Per-path event log, persisted as rows in the `events` SQL table.
//!
//! Events have a typed `kind` and are constructed via the typed helper
//! functions in this module — call sites build events by name (e.g.
//! `event_log::lesson_authored(path, atom)`) rather than by hand.
//!
//! `append` is also the write-through path for the FSRS `cards` cache:
//! when a `QuizAnswered` event lands, the cache row for `(path, quiz)`
//! is folded forward via [`crate::cards::apply_answer_to_cache`]. The
//! event row is the source of truth and the cache is rebuildable via
//! `cards::recompute`.

use chrono::{DateTime, Utc};
use libsql::{Connection, Row, params};
use serde::{Deserialize, Serialize};

use crate::cards;
use crate::types::Rating;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    PathCreated,
    LessonAuthored,
    LessonTaught,
    QuizAuthored,
    QuizPresented,
    QuizAnswered,
    QuizAmended,
    QuizRemoved,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::PathCreated => "path_created",
            EventKind::LessonAuthored => "lesson_authored",
            EventKind::LessonTaught => "lesson_taught",
            EventKind::QuizAuthored => "quiz_authored",
            EventKind::QuizPresented => "quiz_presented",
            EventKind::QuizAnswered => "quiz_answered",
            EventKind::QuizAmended => "quiz_amended",
            EventKind::QuizRemoved => "quiz_removed",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "path_created" => Ok(EventKind::PathCreated),
            "lesson_authored" => Ok(EventKind::LessonAuthored),
            "lesson_taught" => Ok(EventKind::LessonTaught),
            "quiz_authored" => Ok(EventKind::QuizAuthored),
            "quiz_presented" => Ok(EventKind::QuizPresented),
            "quiz_answered" => Ok(EventKind::QuizAnswered),
            "quiz_amended" => Ok(EventKind::QuizAmended),
            "quiz_removed" => Ok(EventKind::QuizRemoved),
            other => Err(Error::CardsCorrupt(format!("unknown event kind: {other}"))),
        }
    }
}

/// JSON-encoded slice of an event beyond what the SQL columns already
/// carry. `rating` lives in its own column (and is omitted here) so
/// FSRS queries can filter by rating without parsing JSON.
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct EventPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<Rating>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_answer: Option<String>,
}

impl EventPayload {
    pub fn is_empty(&self) -> bool {
        self.rating.is_none() && self.reason.is_none() && self.user_answer.is_none()
    }
}

/// Shape sent to the JSON column: rating is stored as its own SQL
/// column, so the JSON blob carries only the free-form fields.
#[derive(Serialize, Default)]
struct StoredPayload<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_answer: Option<&'a str>,
}

impl StoredPayload<'_> {
    fn is_empty(&self) -> bool {
        self.reason.is_none() && self.user_answer.is_none()
    }
}

#[derive(Serialize, Deserialize, Default)]
struct LoadedPayload {
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    user_answer: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Event {
    pub ts: DateTime<Utc>,
    pub kind: EventKind,
    pub path: String,
    pub atom: Option<String>,
    pub quiz: Option<String>,
    pub payload: EventPayload,
}

// ── Typed constructors ────────────────────────────────────────────

pub fn path_created(path: String) -> Event {
    Event {
        ts: Utc::now(),
        kind: EventKind::PathCreated,
        path,
        atom: None,
        quiz: None,
        payload: EventPayload::default(),
    }
}

pub fn lesson_authored(path: String, atom: String) -> Event {
    Event {
        ts: Utc::now(),
        kind: EventKind::LessonAuthored,
        path,
        atom: Some(atom),
        quiz: None,
        payload: EventPayload::default(),
    }
}

pub fn lesson_taught(path: String, atom: String) -> Event {
    Event {
        ts: Utc::now(),
        kind: EventKind::LessonTaught,
        path,
        atom: Some(atom),
        quiz: None,
        payload: EventPayload::default(),
    }
}

pub fn quiz_authored(path: String, atom: String, quiz: String) -> Event {
    Event {
        ts: Utc::now(),
        kind: EventKind::QuizAuthored,
        path,
        atom: Some(atom),
        quiz: Some(quiz),
        payload: EventPayload::default(),
    }
}

pub fn quiz_presented(path: String, atom: String, quiz: String) -> Event {
    Event {
        ts: Utc::now(),
        kind: EventKind::QuizPresented,
        path,
        atom: Some(atom),
        quiz: Some(quiz),
        payload: EventPayload::default(),
    }
}

pub fn quiz_amended(path: String, atom: String, quiz: String) -> Event {
    Event {
        ts: Utc::now(),
        kind: EventKind::QuizAmended,
        path,
        atom: Some(atom),
        quiz: Some(quiz),
        payload: EventPayload::default(),
    }
}

pub fn quiz_removed(path: String, atom: String, quiz: String) -> Event {
    Event {
        ts: Utc::now(),
        kind: EventKind::QuizRemoved,
        path,
        atom: Some(atom),
        quiz: Some(quiz),
        payload: EventPayload::default(),
    }
}

pub fn quiz_answered(
    path: String,
    atom: Option<String>,
    quiz: String,
    rating: Rating,
    user_answer: Option<String>,
) -> Event {
    Event {
        ts: Utc::now(),
        kind: EventKind::QuizAnswered,
        path,
        atom,
        quiz: Some(quiz),
        payload: EventPayload {
            rating: Some(rating),
            user_answer,
            ..Default::default()
        },
    }
}

// ── Append + load ─────────────────────────────────────────────────

/// Insert one event into `events`. On `QuizAnswered`, also fold the
/// rating into the `cards` write-through cache so the scheduler sees
/// the new due date on its next pass.
///
/// The two writes are not wrapped in a single transaction by design:
/// the event log is the source of truth and the cache can always be
/// rebuilt via [`crate::cards::recompute`]. If the cache update fails
/// the event still persists, and a future `recompute` heals the row.
pub async fn append(conn: &Connection, event: &Event) -> Result<()> {
    let stored = StoredPayload {
        reason: event.payload.reason.as_deref(),
        user_answer: event.payload.user_answer.as_deref(),
    };
    let payload_json = if stored.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&stored)?)
    };
    let rating_int = event.payload.rating.map(Rating::as_int);

    conn.execute(
        "INSERT INTO events(ts, kind, path_id, atom_id, quiz_id, rating, payload) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        params![
            event.ts.to_rfc3339(),
            event.kind.as_str(),
            event.path.clone(),
            event.atom.clone(),
            event.quiz.clone(),
            rating_int,
            payload_json,
        ],
    )
    .await?;

    if event.kind == EventKind::QuizAnswered
        && let (Some(quiz), Some(rating)) = (event.quiz.as_deref(), event.payload.rating)
    {
        cards::apply_answer_to_cache(conn, &event.path, quiz, rating, event.ts).await?;
    }
    Ok(())
}

/// Read all events for a path, in chronological order. Empty if the
/// path has never recorded anything.
pub async fn load(conn: &Connection, path_id: &str) -> Result<Vec<Event>> {
    let mut rows = conn
        .query(
            "SELECT ts, kind, path_id, atom_id, quiz_id, rating, payload \
             FROM events WHERE path_id = ? ORDER BY id ASC",
            params![path_id.to_string()],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(row_to_event(&row)?);
    }
    Ok(out)
}

fn row_to_event(row: &Row) -> Result<Event> {
    let ts_str: String = row.get(0)?;
    let kind_str: String = row.get(1)?;
    let path: String = row.get(2)?;
    let atom: Option<String> = row.get(3)?;
    let quiz: Option<String> = row.get(4)?;
    let rating_int: Option<i64> = row.get(5)?;
    let payload_str: Option<String> = row.get(6)?;

    let kind = EventKind::parse(&kind_str)?;
    let ts = DateTime::parse_from_rfc3339(&ts_str)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| Error::BadTimestamp(format!("{ts_str}: {e}")))?;
    let rating = match rating_int {
        Some(v) => Some(
            Rating::from_int(v)
                .ok_or_else(|| Error::CardsCorrupt(format!("bad rating {v} in events")))?,
        ),
        None => None,
    };
    let mut payload = EventPayload {
        rating,
        ..Default::default()
    };
    if let Some(s) = payload_str {
        let parsed: LoadedPayload = serde_json::from_str(&s)?;
        payload.reason = parsed.reason;
        payload.user_answer = parsed.user_answer;
    }

    Ok(Event {
        ts,
        kind,
        path,
        atom,
        quiz,
        payload,
    })
}
