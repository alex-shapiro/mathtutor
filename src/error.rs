use std::path::PathBuf;

use crate::surface::Surface;

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

    #[error("a learning path needs at least 1 target")]
    NoTargets,

    #[error("cycle in target atoms")]
    Cycle,

    #[error("'{0}' is a cluster, not an atom")]
    NotAtom(String),

    /// The selected path does not exist, or no path exists if the command named no path.
    #[error("{}", match .0 {
        Some(id) => format!("learning path '{id}' not found"),
        None => "no learning path exists".to_string(),
    })]
    NoPath(Option<String>),

    #[error(
        "path '{0}' is not loaded in this transaction; mt must load a path before it records events for the path"
    )]
    PathNotLoaded(String),

    #[error(
        "a path_created event for path '{0}' is outside the write that creates the path; mt records path_created only with the path row"
    )]
    PathCreatedOutsideCreation(String),

    #[error(
        "the overlay is not loaded in this view; mt must load the overlay before it reads lessons or quizzes"
    )]
    OverlayNotLoaded,

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

    #[error("database '{db}' at {url} does not exist; check BASERUNNA_DB and BASERUNNA_URL")]
    DatabaseNotFound { db: String, url: String },

    #[error(
        "the Baserunna server at {url} denied BASERUNNA_TOKEN access to database '{db}' ({reason}); check that BASERUNNA_DB names the database that the key is limited to, and that the key is not read-only"
    )]
    KeyDenied {
        db: String,
        url: String,
        reason: String,
    },

    #[error(
        "database '{db}' does not exist at {url}, and BASERUNNA_TOKEN cannot create it ({reason}); check BASERUNNA_DB. Only a key without a database limit and without read-only access can create a database"
    )]
    KeyCannotCreate {
        db: String,
        url: String,
        reason: String,
    },

    #[error(
        "database '{db}' has a '{table}' table whose columns differ from this mt release; check BASERUNNA_DB"
    )]
    TableMismatch { db: String, table: String },

    #[error(
        "database '{db}' holds table '{table}', which Math Tutor does not use, so mt does not add its tables; check BASERUNNA_DB"
    )]
    ForeignTable { db: String, table: String },

    #[error(
        "database '{db}' stores {tables} schema version '{found}', but this mt release uses version '{expected}'"
    )]
    SchemaMismatch {
        db: String,
        tables: crate::schema::TableSet,
        found: String,
        expected: &'static str,
    },

    #[error("database '{db}' has no {tables} tables")]
    TablesMissing {
        db: String,
        tables: crate::schema::TableSet,
    },

    #[error("malformed row: {0}")]
    BadRow(String),

    #[error("cannot write to stdout: {0}")]
    Stdout(std::io::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

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

    #[error(
        "MT_API_KEY has fewer than {min} characters; set a random key, such as the output of `openssl rand -base64 32`"
    )]
    WeakApiKey { min: usize },

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
    NoTargets = "no_targets" => 2,
    Cycle = "cycle" => 2,
    NotAtom = "not_atom" => 2,
    SubpathNotTopDown = "subpath_not_top_down" => 2,
    SubpathEmpty = "subpath_empty" => 2,
    SubpathTailNotTarget = "subpath_tail_not_target" => 2,
    SubpathDuplicateAtom = "subpath_duplicate_atom" => 2,
    ConflictingFlags = "conflicting_flags" => 2,
    /// A missing or incorrect setting, such as an absent token or database.
    Config = "config" => 2,
    NoPath = "no_path" => 1,
    NoLesson = "no_lesson" => 1,
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
            Error::NoTargets => ErrorKind::NoTargets,
            Error::Cycle => ErrorKind::Cycle,
            Error::NotAtom(_) => ErrorKind::NotAtom,
            Error::SubpathNotTopDown => ErrorKind::SubpathNotTopDown,
            Error::SubpathEmpty => ErrorKind::SubpathEmpty,
            Error::SubpathTailNotTarget(_) => ErrorKind::SubpathTailNotTarget,
            Error::SubpathDuplicateAtom(_) => ErrorKind::SubpathDuplicateAtom,
            Error::ConflictingFlags(_, _) => ErrorKind::ConflictingFlags,
            Error::MissingToken
            | Error::TokenRejected { .. }
            | Error::KeyDenied { .. }
            | Error::KeyCannotCreate { .. }
            | Error::DatabaseNotFound { .. }
            | Error::TableMismatch { .. }
            | Error::ForeignTable { .. }
            | Error::MissingTursoUrl
            | Error::MissingAuth
            | Error::WeakApiKey { .. }
            | Error::BadBindAddr(_)
            | Error::BadPublicUrl(_)
            | Error::BadClientIpHeader(_) => ErrorKind::Config,
            Error::NoPath(_) => ErrorKind::NoPath,
            Error::NoLesson(_) => ErrorKind::NoLesson,
            Error::FileIo { .. }
            | Error::AymlParse { .. }
            | Error::AymlSerialize(_)
            | Error::Stdout(_) => ErrorKind::Io,
            Error::GraphCheckFailed => ErrorKind::GraphCheck,
            Error::Baserunna(baserunna_client::Error::Decode { .. })
            | Error::BadRow(_)
            | Error::CardsCorrupt(_) => ErrorKind::Corrupt,
            Error::Baserunna(_) | Error::SchemaMismatch { .. } | Error::TablesMissing { .. } => {
                ErrorKind::Database
            }
            Error::Json(_) => ErrorKind::Json,
            Error::Fsrs(_) => ErrorKind::Fsrs,
            Error::Turso(_)
            | Error::ImportMismatch(_)
            | Error::ImportAlreadyComplete
            | Error::ImportTargetNotEmpty
            | Error::ImportInProgress => ErrorKind::Import,
            Error::Http(_) => ErrorKind::Http,
            Error::Bind { .. } | Error::Serve(_) => ErrorKind::Server,
            Error::PathNotLoaded(_)
            | Error::PathCreatedOutsideCreation(_)
            | Error::OverlayNotLoaded => ErrorKind::Internal,
        }
    }

    /// Return the text of this error for an agent on `surface`, with the next step if the error names a missing path.
    pub fn message(&self, surface: Surface) -> String {
        let step = match (self, surface) {
            (Error::NoPath(Some(_)), Surface::Cli) => "run `mt path list` to list the paths",
            (Error::NoPath(Some(_)), Surface::Mcp) => "call `get_paths` to list the paths",
            (Error::NoPath(None), Surface::Cli) => "run `mt path new` to create one",
            (Error::NoPath(None), Surface::Mcp) => "call `new_path` to create one",
            _ => return self.to_string(),
        };
        format!("{self}; {step}")
    }

    /// Return `true` if this error reports that the reader of stdout closed it, as `head` does after its last line.
    pub fn is_closed_stdout(&self) -> bool {
        matches!(self, Error::Stdout(e) if e.kind() == std::io::ErrorKind::BrokenPipe)
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
        assert_eq!(Error::NoPath(None).exit_code(), 1);
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
    fn a_missing_path_names_its_id_and_the_list_step_of_the_surface() {
        let missing = Error::NoPath(Some("p_2026_05_09_173042_k3x9".into()));
        assert_eq!(
            missing.message(Surface::Cli),
            "learning path 'p_2026_05_09_173042_k3x9' not found; run `mt path list` to list the paths"
        );
        assert!(
            missing
                .message(Surface::Mcp)
                .ends_with("call `get_paths` to list the paths")
        );
        assert!(
            Error::NoPath(None)
                .message(Surface::Mcp)
                .contains("`new_path`")
        );
        assert_eq!(Error::Cycle.message(Surface::Cli), Error::Cycle.to_string());
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
