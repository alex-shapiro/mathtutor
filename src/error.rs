//! Crate-wide unified error type.
//!
//! Every fallible function in this binary returns `crate::Result<T>` —
//! one enum, one Display, one shape. Per-module error types used to be
//! a thing here; they ended up mostly wrapping each other via `#[from]`,
//! adding noise without buying behavioral granularity (nothing in
//! `main.rs` pattern-matches on which subsystem failed). Single Error
//! keeps the type system out of the way of a CLI that only ever prints
//! and exits.

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    // I/O — `path` identifies the offending file (real or `<embedded>/…`).
    #[error("io: {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    // AYML — keep `path` for parser diagnostics; serialization rarely
    // benefits from one (it's failing on in-memory data).
    #[error("ayml parse: {path}: {message}")]
    AymlParse { path: PathBuf, message: String },
    #[error("ayml serialize: {0}")]
    AymlSerialize(String),

    // Curriculum graph semantics.
    #[error("unknown id: {0}")]
    UnknownId(String),
    #[error("cluster '{0}' has no atomic descendants")]
    EmptyCluster(String),
    #[error("cycle in target atoms")]
    Cycle,
    #[error("'{0}' is a cluster, not an atom")]
    NotAtom(String),
    #[error("atom '{0}' not found in graph")]
    AtomNotFound(String),

    // Per-path state lookup.
    #[error("no learning path found (run `mt new` first)")]
    NoPath,
    #[error("HOME not set; set MATHTUTOR_HOME or HOME")]
    NoHome,

    // Authoring preconditions.
    #[error("atom '{0}' already has a stored lesson")]
    LessonAlreadyExists(String),
    #[error("atom '{0}' has no stored lesson; teach it before authoring quizzes")]
    NoLesson(String),

    // FSRS.
    #[error("fsrs: {0}")]
    Fsrs(String),

    // SQL / libsql.
    #[error(transparent)]
    Db(#[from] libsql::Error),

    // JSON payload for the `events.payload` column.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    // Bad timestamp coming back out of the database.
    #[error("bad timestamp: {0}")]
    BadTimestamp(String),

    // Out-of-range integer in `events.rating` or any other column that
    // round-trips a `Rating`.
    #[error("invalid rating value: {0}")]
    InvalidRating(i64),

    // Unrecognized string read out of an overlay table column that
    // round-trips a `Difficulty` or `QuizType`.
    #[error("invalid difficulty: {0}")]
    InvalidDifficulty(String),
    #[error("invalid quiz kind: {0}")]
    InvalidQuizKind(String),

    // Cards cache row missing its expected columns.
    #[error("cards cache corrupt: {0}")]
    CardsCorrupt(String),

    // Unrecognized event kind read out of the `events.kind` column.
    #[error("unknown event kind: {0}")]
    UnknownEventKind(String),
}
