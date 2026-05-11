//! Per-path event log: append-only AYML record.
//!
//! Events have a typed `kind` and are constructed via the typed helper
//! functions in this module — call sites build events by name (e.g.
//! `event_log::lesson_authored(path, atom)`) rather than by hand.

use std::fs::{self, File};
use std::io::BufReader;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::path::path_dir;
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

#[derive(Debug, Serialize, Deserialize)]
pub struct Event {
    pub ts: DateTime<Utc>,
    #[serde(rename = "type")]
    pub kind: EventKind,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub atom: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiz: Option<String>,
    #[serde(default, skip_serializing_if = "EventPayload::is_empty")]
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

pub fn append(event: Event) -> Result<()> {
    let dir = path_dir(&event.path)?;
    fs::create_dir_all(&dir).map_err(|e| Error::Io {
        path: dir.clone(),
        source: e,
    })?;
    let log_file = dir.join("log.ayml");

    let mut events = read_events(&log_file)?;
    events.push(event);

    let text = ayml::to_string(&events).map_err(|e| Error::AymlSerialize(e.to_string()))?;
    fs::write(&log_file, text).map_err(|e| Error::Io {
        path: log_file,
        source: e,
    })
}

/// Read all events for a path, in chronological order. Empty if the log
/// doesn't exist yet.
pub fn load(path_id: &str) -> Result<Vec<Event>> {
    read_events(&path_dir(path_id)?.join("log.ayml"))
}

fn read_events(log_file: &std::path::Path) -> Result<Vec<Event>> {
    if !log_file.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(log_file).map_err(|e| Error::Io {
        path: log_file.to_path_buf(),
        source: e,
    })?;
    let metadata = file.metadata().map_err(|e| Error::Io {
        path: log_file.to_path_buf(),
        source: e,
    })?;
    if metadata.len() == 0 {
        return Ok(Vec::new());
    }
    ayml::from_reader(BufReader::new(file)).map_err(|e| Error::AymlParse {
        path: log_file.to_path_buf(),
        message: e.to_string(),
    })
}
