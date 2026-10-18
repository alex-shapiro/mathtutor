//! Per-path event log, stored as the stream `events.<path id>`.
//!
//! Each event has a typed `kind` and is constructed via the typed helper
//! functions in this module. [`crate::view::Changes::append`] logs an event
//! and updates the rows derived from it in the same transaction.

use chrono::{DateTime, Utc};
use serde_json::{Value as Json, json};

use crate::db::{self, Db};
use crate::schema;
use crate::types::Rating;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::PathCreated => "path_created",
            EventKind::LessonAuthored => "lesson_authored",
            EventKind::LessonAmended => "lesson_amended",
            EventKind::LessonTaught => "lesson_taught",
            EventKind::QuizAuthored => "quiz_authored",
            EventKind::QuizPresented => "quiz_presented",
            EventKind::QuizAnswered => "quiz_answered",
            EventKind::QuizAmended => "quiz_amended",
            EventKind::QuizRemoved => "quiz_removed",
        }
    }
}

impl std::fmt::Display for EventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for EventKind {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "path_created" => Ok(EventKind::PathCreated),
            "lesson_authored" => Ok(EventKind::LessonAuthored),
            "lesson_amended" => Ok(EventKind::LessonAmended),
            "lesson_taught" => Ok(EventKind::LessonTaught),
            "quiz_authored" => Ok(EventKind::QuizAuthored),
            "quiz_presented" => Ok(EventKind::QuizPresented),
            "quiz_answered" => Ok(EventKind::QuizAnswered),
            "quiz_amended" => Ok(EventKind::QuizAmended),
            "quiz_removed" => Ok(EventKind::QuizRemoved),
            other => Err(Error::UnknownEventKind(other.to_string())),
        }
    }
}

/// Optional fields beyond the event's kind, path, atom and quiz.
#[derive(Debug, Default, Clone)]
pub struct EventPayload {
    pub rating: Option<Rating>,
    pub reason: Option<String>,
    pub user_answer: Option<String>,
}

impl EventPayload {
    pub fn is_empty(&self) -> bool {
        self.rating.is_none() && self.reason.is_none() && self.user_answer.is_none()
    }
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

// ── Encoding + load ───────────────────────────────────────────────

/// An event as a stream record body.
pub fn encode(e: &Event) -> Json {
    let mut j = json!({ "ts": db::format_ts(e.ts), "kind": e.kind.as_str(), "path": e.path });
    let mut set = |k: &str, v: Option<Json>| {
        if let Some(v) = v {
            j[k] = v;
        }
    };
    set("atom", e.atom.as_deref().map(Json::from));
    set("quiz", e.quiz.as_deref().map(Json::from));
    set("rating", e.payload.rating.map(|r| Json::from(i64::from(r))));
    set("reason", e.payload.reason.as_deref().map(Json::from));
    set(
        "user_answer",
        e.payload.user_answer.as_deref().map(Json::from),
    );
    j
}

/// The event a stream record body holds.
pub fn decode(j: &Json) -> Result<Event> {
    let s = |k: &str| j[k].as_str().map(str::to_string);
    let required = |k: &str| s(k).ok_or_else(|| Error::BadRow(format!("event.{k}: {j}")));
    Ok(Event {
        ts: db::parse_ts(&required("ts")?)?,
        kind: required("kind")?.parse()?,
        path: required("path")?,
        atom: s("atom"),
        quiz: s("quiz"),
        payload: EventPayload {
            rating: j["rating"].as_i64().map(Rating::try_from).transpose()?,
            reason: s("reason"),
            user_answer: s("user_answer"),
        },
    })
}

/// Read all events for a path, in the order they were logged.
pub async fn load(db: &Db, path_id: &str) -> Result<Vec<Event>> {
    let stream = schema::events_stream(path_id);
    let mut out = Vec::new();
    let mut from = 0;
    loop {
        let op = json!({ "op": "read_stream", "stream": stream, "from_seq": from, "limit": 1000 });
        let r = db.read(&[op]).await?.swap_remove(0);
        for rec in r["records"].as_array().into_iter().flatten() {
            out.push(decode(&rec["json"])?);
        }
        if r["done"] != false {
            return Ok(out);
        }
        from = r["next_seq"]
            .as_u64()
            .ok_or_else(|| Error::BadResponse(format!("next_seq: {r}")))?;
    }
}
