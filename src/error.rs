//! Crate-wide unified error type

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// File IO error
    #[error("io: {path}: {source}")]
    FileIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// AYML parse error
    #[error("ayml parse: {path}: {message}")]
    AymlParse { path: PathBuf, message: String },

    /// AYML serialization error
    #[error("ayml serialize: {0}")]
    AymlSerialize(String),

    /// Unknown ID for a graph node
    #[error("unknown id: {0}")]
    UnknownId(String),

    /// Empty graph node cluster
    #[error("cluster '{0}' has no atomic descendants")]
    EmptyCluster(String),

    /// Graph cycle
    #[error("cycle in target atoms")]
    Cycle,

    /// Received a cluster instead of an atom
    #[error("'{0}' is a cluster, not an atom")]
    NotAtom(String),

    /// Atom not found
    #[error("atom '{0}' not found in graph")]
    AtomNotFound(String),

    /// Learning path not found
    #[error("learning path not found")]
    NoPath,

    /// Missing a home directory
    #[error("HOME not set; set MATHTUTOR_HOME or HOME")]
    NoHome,

    /// A lesson does not exist
    #[error("atom '{0}' has no stored lesson; teach it before authoring quizzes")]
    NoLesson(String),

    // FSRS error
    #[error("fsrs: {0}")]
    Fsrs(String),

    /// libsql error
    #[error(transparent)]
    Db(#[from] libsql::Error),

    /// JSON payload for the `events.payload` column
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    /// Bad timestamp coming back out of the database
    #[error("bad timestamp: {0}")]
    BadTimestamp(String),

    /// Out-of-range integer for `events.rating`
    #[error("invalid rating value: {0}")]
    InvalidRating(i64),

    /// Unrecognized string for  [`crate::types::Difficulty`]
    #[error("invalid difficulty: {0}")]
    InvalidDifficulty(String),

    /// Unrecognized string for [`crate::types::QuizType`]
    #[error("invalid quiz type: {0}")]
    InvalidQuizType(String),

    // Cards cache row missing its expected columns.
    #[error("cards cache corrupt: {0}")]
    CardsCorrupt(String),

    // Unrecognized event kind read out of the `events.kind` column.
    #[error("unknown event kind: {0}")]
    UnknownEventKind(String),

    /// MCP started with neither `MT_API_KEY` nor `MT_ADMIN_PASSWORD` set
    #[error("missing MT_API_KEY or MT_ADMIN_PASSWORD")]
    MissingAuth,

    /// MCP address did not parse as a socket address
    #[error("invalid bind address '{0}'")]
    BadBindAddr(String),

    /// `MT_PUBLIC_URL` did not parse as a URL
    #[error("invalid public URL '{0}'")]
    BadPublicUrl(String),

    /// MCP server failed to bind to the resolved socket
    #[error("bind {addr}: {source}")]
    Bind {
        addr: String,
        #[source]
        source: std::io::Error,
    },

    /// MCP server returned returned an error when accepting a connection
    #[error("mcp server: {0}")]
    Serve(#[source] std::io::Error),
}
