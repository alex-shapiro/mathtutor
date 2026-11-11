use std::path::PathBuf;

use crate::types::{Difficulty, QuizType, Rating, Strategy};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {path}: {source}")]
    FileIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("ayml parse: {path}: {message}")]
    AymlParse { path: PathBuf, message: String },

    #[error("the curriculum check failed; see the issues above")]
    GraphCheckFailed,

    #[error("ayml serialize: {0}")]
    AymlSerialize(String),

    #[error("unknown id: {0}")]
    UnknownId(String),

    #[error("cluster '{0}' does not have atomic descendants")]
    EmptyCluster(String),

    #[error("cycle in target atoms")]
    Cycle,

    #[error("'{0}' is a cluster, not an atom")]
    NotAtom(String),

    #[error("atom '{0}' not found in graph")]
    AtomNotFound(String),

    #[error("learning path not found")]
    NoPath,

    #[error(
        "path '{0}' is not loaded in this transaction; mt must load a path before it records events for the path"
    )]
    PathNotLoaded(String),

    #[error(
        "the overlay is not loaded in this view; mt must load the overlay before it reads lessons or quizzes"
    )]
    OverlayNotLoaded,

    #[error("HOME not set; set MATHTUTOR_HOME or HOME")]
    NoHome,

    #[error("atom '{0}' does not have a stored lesson; teach it before authoring quizzes")]
    NoLesson(String),

    #[error("fsrs: {0}")]
    Fsrs(String),

    #[error("baserunna: {0}")]
    Baserunna(#[from] baserunna_client::Error),

    #[error("http: {0}")]
    Http(#[from] reqwest::Error),

    #[error("BASERUNNA_TOKEN not set; create an API key on the Baserunna server")]
    MissingToken,

    #[error(
        "the Baserunna server at {url} rejected BASERUNNA_TOKEN; check that BASERUNNA_URL names the server that issued the key and that BASERUNNA_TOKEN is a current key"
    )]
    TokenRejected { url: String },

    #[error(
        "database '{db}' at {url} does not exist or does not hold Math Tutor data; check BASERUNNA_DB and BASERUNNA_URL"
    )]
    DatabaseNotFound { db: String, url: String },

    #[error(
        "database '{db}' has a '{table}' table whose columns differ from this mt release; check BASERUNNA_DB"
    )]
    TableMismatch { db: String, table: String },

    #[error(
        "database '{db}' stores {tables} schema version '{found}', but this mt release uses version '{expected}'"
    )]
    SchemaMismatch {
        db: String,
        tables: crate::schema::TableSet,
        found: String,
        expected: &'static str,
    },

    #[error("database '{db}' has no authorization tables; restart mt mcp to create them")]
    AuthTablesMissing { db: String },

    #[error("malformed row: {0}")]
    BadRow(String),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("invalid rating '{0}' (expected {expected})", expected = Rating::SPELLINGS.join(" | "))]
    InvalidRating(String),

    #[error("invalid difficulty '{0}' (expected {expected})", expected = Difficulty::SPELLINGS.join(" | "))]
    InvalidDifficulty(String),

    #[error("invalid quiz type '{0}' (expected {expected})", expected = QuizType::SPELLINGS.join(" | "))]
    InvalidQuizType(String),

    #[error("invalid strategy '{0}' (expected {expected})", expected = Strategy::SPELLINGS.join(" | "))]
    InvalidStrategy(String),

    #[error("subpaths apply only to paths with the top_down strategy")]
    SubpathNotTopDown,

    #[error("subpath must not be empty")]
    SubpathEmpty,

    #[error("subpath must end in a target atom; '{0}' is not a target of this path")]
    SubpathTailNotTarget(String),

    #[error("atom '{0}' appears more than once in the subpath")]
    SubpathDuplicateAtom(String),

    #[error("card row corrupt: {0}")]
    CardsCorrupt(String),

    #[error("TURSO_URL not set; pass --url")]
    MissingTursoUrl,

    #[error("turso: {0}")]
    Turso(String),

    #[error(
        "{0} differences between the import and libSQL; learner commands stay blocked. Delete the database with an unscoped Baserunna key, fix the cause, and run `mt import-turso` again"
    )]
    ImportMismatch(usize),

    #[error("this database already completed an import; a repeat run would revert newer data")]
    ImportAlreadyComplete,

    #[error("the import target already holds data; import into an empty database")]
    ImportTargetNotEmpty,

    #[error(
        "learner data is unavailable while an operator finishes a migration of this database; try again later"
    )]
    ImportInProgress,

    #[error("flags {0} and {1} are mutually exclusive")]
    ConflictingFlags(&'static str, &'static str),

    #[error("missing MT_API_KEY or MT_ADMIN_PASSWORD")]
    MissingAuth,

    #[error("invalid bind address '{0}'")]
    BadBindAddr(String),

    #[error("invalid public URL '{0}'")]
    BadPublicUrl(String),

    #[error("invalid client IP header name '{0}'")]
    BadClientIpHeader(String),

    #[error("bind {addr}: {source}")]
    Bind {
        addr: String,
        #[source]
        source: std::io::Error,
    },

    #[error("mcp server: {0}")]
    Serve(#[source] std::io::Error),
}

/// Define [`ErrorKind`] from one row per kind: variant, MCP name, and exit code.
macro_rules! error_kinds {
    ($($(#[doc = $doc:literal])* $kind:ident = $name:literal => $code:literal,)*) => {
        /// Class of an error, which sets the exit code of `mt` and the `kind` of an MCP tool error.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum ErrorKind {
            $($(#[doc = $doc])* $kind,)*
        }

        impl ErrorKind {
            /// Every error kind.
            pub const ALL: &[ErrorKind] = &[$(ErrorKind::$kind,)*];

            /// Return the `kind` name of an MCP tool error of this kind.
            pub fn name(self) -> &'static str {
                match self {
                    $(ErrorKind::$kind => $name,)*
                }
            }

            /// Return the process exit code of this kind, as `docs/design.md` documents it.
            pub fn exit_code(self) -> u8 {
                match self {
                    $(ErrorKind::$kind => $code,)*
                }
            }
        }
    };
}

error_kinds! {
    UnknownId = "unknown_id" => 2,
    EmptyCluster = "empty_cluster" => 2,
    Cycle = "cycle" => 2,
    NotAtom = "not_atom" => 2,
    AtomNotFound = "atom_not_found" => 2,
    InvalidRating = "invalid_rating" => 2,
    InvalidDifficulty = "invalid_difficulty" => 2,
    InvalidQuizType = "invalid_quiz_type" => 2,
    InvalidStrategy = "invalid_strategy" => 2,
    SubpathNotTopDown = "subpath_not_top_down" => 2,
    SubpathEmpty = "subpath_empty" => 2,
    SubpathTailNotTarget = "subpath_tail_not_target" => 2,
    SubpathDuplicateAtom = "subpath_duplicate_atom" => 2,
    ConflictingFlags = "conflicting_flags" => 2,
    /// A missing or incorrect setting, such as an absent token or database.
    Config = "config" => 2,
    NoPath = "no_path" => 1,
    NoLesson = "no_lesson" => 1,
    NoHome = "no_home" => 1,
    Io = "io" => 1,
    GraphCheck = "graph_check" => 1,
    /// A stored row or response that `mt` cannot decode.
    Corrupt = "corrupt" => 1,
    Database = "database" => 1,
    Json = "json" => 1,
    Fsrs = "fsrs" => 1,
    Import = "import" => 1,
    Http = "http" => 1,
    Server = "server" => 1,
    Internal = "internal" => 1,
    /// The command may have committed its writes.
    OutcomeUnknown = "outcome_unknown" => 3,
}

impl Error {
    /// Return the class of this error.
    pub fn kind(&self) -> ErrorKind {
        if self.outcome_is_unknown() {
            return ErrorKind::OutcomeUnknown;
        }
        match self {
            Error::UnknownId(_) => ErrorKind::UnknownId,
            Error::EmptyCluster(_) => ErrorKind::EmptyCluster,
            Error::Cycle => ErrorKind::Cycle,
            Error::NotAtom(_) => ErrorKind::NotAtom,
            Error::AtomNotFound(_) => ErrorKind::AtomNotFound,
            Error::InvalidRating(_) => ErrorKind::InvalidRating,
            Error::InvalidDifficulty(_) => ErrorKind::InvalidDifficulty,
            Error::InvalidQuizType(_) => ErrorKind::InvalidQuizType,
            Error::InvalidStrategy(_) => ErrorKind::InvalidStrategy,
            Error::SubpathNotTopDown => ErrorKind::SubpathNotTopDown,
            Error::SubpathEmpty => ErrorKind::SubpathEmpty,
            Error::SubpathTailNotTarget(_) => ErrorKind::SubpathTailNotTarget,
            Error::SubpathDuplicateAtom(_) => ErrorKind::SubpathDuplicateAtom,
            Error::ConflictingFlags(_, _) => ErrorKind::ConflictingFlags,
            Error::MissingToken
            | Error::TokenRejected { .. }
            | Error::DatabaseNotFound { .. }
            | Error::TableMismatch { .. }
            | Error::MissingTursoUrl
            | Error::MissingAuth
            | Error::BadBindAddr(_)
            | Error::BadPublicUrl(_)
            | Error::BadClientIpHeader(_) => ErrorKind::Config,
            Error::NoPath => ErrorKind::NoPath,
            Error::NoLesson(_) => ErrorKind::NoLesson,
            Error::NoHome => ErrorKind::NoHome,
            Error::FileIo { .. } | Error::AymlParse { .. } | Error::AymlSerialize(_) => {
                ErrorKind::Io
            }
            Error::GraphCheckFailed => ErrorKind::GraphCheck,
            Error::Baserunna(baserunna_client::Error::Decode { .. })
            | Error::BadRow(_)
            | Error::CardsCorrupt(_) => ErrorKind::Corrupt,
            Error::Baserunna(_)
            | Error::SchemaMismatch { .. }
            | Error::AuthTablesMissing { .. } => ErrorKind::Database,
            Error::Json(_) => ErrorKind::Json,
            Error::Fsrs(_) => ErrorKind::Fsrs,
            Error::Turso(_)
            | Error::ImportMismatch(_)
            | Error::ImportAlreadyComplete
            | Error::ImportTargetNotEmpty
            | Error::ImportInProgress => ErrorKind::Import,
            Error::Http(_) => ErrorKind::Http,
            Error::Bind { .. } | Error::Serve(_) => ErrorKind::Server,
            Error::PathNotLoaded(_) | Error::OverlayNotLoaded => ErrorKind::Internal,
        }
    }

    /// Return the process exit code of this error.
    pub fn exit_code(&self) -> u8 {
        self.kind().exit_code()
    }

    /// Mark a failure from a read-only or setup step as not applied.
    #[must_use]
    pub fn into_not_applied(self) -> Self {
        match self {
            Error::Baserunna(e) if e.outcome() == baserunna_client::api::Outcome::Unknown => {
                Error::Baserunna(baserunna_client::Error::NotApplied(Box::new(e)))
            }
            other => other,
        }
    }

    /// Return `true` if the failed command may have committed its writes.
    pub fn outcome_is_unknown(&self) -> bool {
        matches!(
            self,
            Error::Baserunna(e) if e.outcome() == baserunna_client::api::Outcome::Unknown
        )
    }
}

impl baserunna_client::TxnError for Error {
    fn client_error(&self) -> Option<&baserunna_client::Error> {
        match self {
            Error::Baserunna(e) => Some(e),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use baserunna_client::TransportError;

    #[test]
    fn exit_codes_separate_usage_state_and_unknown_outcomes() {
        assert_eq!(Error::UnknownId("x".into()).exit_code(), 2);
        assert_eq!(Error::NoPath.exit_code(), 1);
        let unsent = baserunna_client::Error::Transport(TransportError::NotSent("refused".into()));
        assert_eq!(Error::Baserunna(unsent).exit_code(), 1);
        let lost = baserunna_client::Error::Transport(TransportError::Unknown("reset".into()));
        let lost = Error::Baserunna(lost);
        assert!(lost.outcome_is_unknown());
        assert_eq!(lost.exit_code(), 3);
    }

    #[test]
    fn every_kind_has_the_exit_code_that_the_design_doc_lists() {
        let doc = include_str!("../docs/design.md");
        let documented: Vec<(String, u8)> = doc
            .lines()
            .filter_map(|line| {
                let cells: Vec<&str> = line.split('|').map(str::trim).collect();
                let name = cells.get(1)?.strip_prefix('`')?.strip_suffix('`')?;
                let code = cells.get(2)?.parse().ok()?;
                Some((name.to_string(), code))
            })
            .collect();
        let kinds: Vec<(String, u8)> = ErrorKind::ALL
            .iter()
            .map(|k| (k.name().to_string(), k.exit_code()))
            .collect();
        assert_eq!(documented, kinds);
    }

    #[test]
    fn open_import_error_gives_learners_no_operator_steps() {
        let shown = Error::ImportInProgress.to_string();
        assert!(shown.contains("migration"), "{shown}");
        assert!(!shown.contains("mt "), "{shown}");
        assert!(
            Error::ImportMismatch(2)
                .to_string()
                .contains("mt import-turso")
        );
    }
}
