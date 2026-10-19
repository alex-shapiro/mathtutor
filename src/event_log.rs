//! Per-path event log, stored as the stream `events.<path id>`.
//!
//! Each event has a typed `kind` and is constructed via the typed helper
//! functions in this module. [`crate::view::Changes::append`] logs an event
//! and updates the rows derived from it in the same transaction.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::Result;
use crate::db::{Db, Op, ts};
use crate::schema;
use crate::types::Rating;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
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

/// Optional fields beyond the event's kind, path, atom and quiz.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
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

/// One logged event; its serialized form is its stream record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    #[serde(with = "ts")]
    pub ts: DateTime<Utc>,
    pub kind: EventKind,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub atom: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiz: Option<String>,
    #[serde(flatten)]
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

pub fn lesson_amended(path: String, atom: String) -> Event {
    Event {
        ts: Utc::now(),
        kind: EventKind::LessonAmended,
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

// ── Load ──────────────────────────────────────────────────────────

/// Read all events for a path, in the order they were logged.
pub async fn load(db: &Db, path_id: &str) -> Result<Vec<Event>> {
    let mut op = Op::read_stream(schema::events_stream(path_id), 0);
    let mut out = Vec::new();
    loop {
        let page = db.read_one(op.clone()).await?;
        let next = op.next_page(&page);
        out.extend(page.records::<Event>()?);
        match next {
            Some(n) => op = n,
            None => return Ok(out),
        }
    }
}
