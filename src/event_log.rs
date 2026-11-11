//! Per-path `events.<path id>` learning event stream.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::db::client::api::ErrorCode;
use crate::db::{Db, Op, ts};
use crate::schema;
use crate::types::Rating;
use crate::{Error, Result};

/// Learning event in the stream of its path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    #[serde(with = "ts")]
    pub ts: DateTime<Utc>,
    pub path: String,
    #[serde(flatten)]
    pub kind: EventKind,
}

/// Kind of a learning event and the fields that the kind records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EventKind {
    PathCreated {},
    LessonAuthored {
        atom: String,
    },
    LessonAmended {
        atom: String,
    },
    LessonTaught {
        atom: String,
    },
    QuizAuthored {
        atom: String,
        quiz: String,
    },
    QuizPresented {
        atom: String,
        quiz: String,
    },
    QuizAnswered {
        /// Atom of the quiz, or `None` in answers recorded before the log stored it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        atom: Option<String>,
        quiz: String,
        rating: Rating,
        /// Verbatim answer of the user.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_answer: Option<String>,
    },
    QuizAmended {
        atom: String,
        quiz: String,
    },
    QuizRemoved {
        atom: String,
        quiz: String,
    },
}

impl Event {
    /// Return an event of `kind` on `path` at the current time.
    pub fn now(path: impl Into<String>, kind: EventKind) -> Self {
        Self {
            ts: Utc::now(),
            path: path.into(),
            kind,
        }
    }
}

/// Return the events of `path_id` in order, or an empty list if the event stream does not exist.
pub async fn load(db: &Db, path_id: &str) -> Result<Vec<Event>> {
    let mut op = Op::read_stream(schema::events_stream(path_id), 0);
    let mut out = Vec::new();
    loop {
        let page = match db.read_one(op.clone()).await {
            Err(Error::Baserunna(e)) if e.code() == Some(ErrorCode::NotFound) => {
                db.read(&[]).await?;
                return Ok(out);
            }
            other => other?,
        };
        let next = op.next_page(&page);
        out.extend(page.records::<Event>()?);
        match next {
            Some(n) => op = n,
            None => return Ok(out),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stored_fields_follow_the_kind() {
        let e = Event {
            ts: ts::parse("2026-01-01T00:00:00Z").unwrap(),
            path: "p".into(),
            kind: EventKind::QuizAnswered {
                atom: None,
                quiz: "a.q1".into(),
                rating: Rating::Good,
                user_answer: Some("42".into()),
            },
        };
        let stored = json!({
            "ts": "2026-01-01T00:00:00.000Z",
            "path": "p",
            "kind": "quiz_answered",
            "quiz": "a.q1",
            "rating": "good",
            "user_answer": "42",
        });
        assert_eq!(serde_json::to_value(&e).unwrap(), stored);
        assert_eq!(serde_json::from_value::<Event>(stored).unwrap(), e);
        let created =
            json!({"ts": "2026-01-01T00:00:00.000Z", "path": "p", "kind": "path_created"});
        let e: Event = serde_json::from_value(created).unwrap();
        assert_eq!(e.kind, EventKind::PathCreated {});
    }

    #[test]
    fn fields_that_the_kind_does_not_record_are_rejected() {
        let event = |fields: serde_json::Value| {
            let mut e = json!({"ts": "2026-01-01T00:00:00.000Z", "path": "p"});
            e.as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            serde_json::from_value::<Event>(e)
        };
        assert!(event(json!({"kind": "quiz_answered", "quiz": "a.q1"})).is_err());
        assert!(event(json!({"kind": "lesson_taught"})).is_err());
        assert!(event(json!({"kind": "lesson_taught", "atom": "a", "quiz": "a.q1"})).is_err());
        assert!(event(json!({"kind": "path_created", "reason": "x"})).is_err());
        assert!(event(json!({"kind": "lesson_taught", "atom": "a"})).is_ok());
    }
}
