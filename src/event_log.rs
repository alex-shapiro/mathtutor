//! Per-path event log: append-only AYML record.

use std::fs;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::path::{PathError, Rating, path_dir};

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct EventPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<Rating>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl EventPayload {
    pub fn is_empty(&self) -> bool {
        self.rating.is_none() && self.reason.is_none()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Event {
    pub ts: DateTime<Utc>,
    #[serde(rename = "type")]
    pub kind: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub atom: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiz: Option<String>,
    #[serde(default, skip_serializing_if = "EventPayload::is_empty")]
    pub payload: EventPayload,
}

pub fn append(event: Event) -> Result<(), PathError> {
    let dir = path_dir(&event.path)?;
    fs::create_dir_all(&dir)?;
    let log_file = dir.join("log.ayml");

    let mut events: Vec<Event> = if log_file.exists() {
        let text = fs::read_to_string(&log_file)?;
        if text.trim().is_empty() {
            Vec::new()
        } else {
            ayml::from_str(&text).map_err(|e| PathError::Parse(e.to_string()))?
        }
    } else {
        Vec::new()
    };
    events.push(event);

    let text = ayml::to_string(&events).map_err(|e| PathError::Serialize(e.to_string()))?;
    fs::write(log_file, text)?;
    Ok(())
}

/// Read all events for a path, in chronological order. Empty if the log
/// doesn't exist yet.
pub fn load(path_id: &str) -> Result<Vec<Event>, PathError> {
    let log_file = path_dir(path_id)?.join("log.ayml");
    if !log_file.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(&log_file)?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    ayml::from_str(&text).map_err(|e| PathError::Parse(e.to_string()))
}
