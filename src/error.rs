use std::path::PathBuf;

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

    #[error("ayml serialize: {0}")]
    AymlSerialize(String),

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

    #[error("learning path not found")]
    NoPath,

    #[error("HOME not set; set MATHTUTOR_HOME or HOME")]
    NoHome,

    #[error("atom '{0}' has no stored lesson; teach it before authoring quizzes")]
    NoLesson(String),

    #[error("fsrs: {0}")]
    Fsrs(String),

    #[error("baserunna: {0}")]
    Api(crate::db::ApiError),

    #[error("baserunna: {0}")]
    Http(#[from] reqwest::Error),

    #[error("BASERUNNA_TOKEN not set; create an API key on the Baserunna server")]
    MissingToken,

    #[error("unexpected response: {0}")]
    BadResponse(String),

    #[error("malformed row: {0}")]
    BadRow(String),

    #[error("malformed event: {0}")]
    BadEvent(&'static str),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("invalid rating value: {0}")]
    InvalidRating(i64),

    #[error("invalid difficulty: {0}")]
    InvalidDifficulty(String),

    #[error("invalid quiz type: {0}")]
    InvalidQuizType(String),

    #[error("invalid strategy: {0}")]
    InvalidStrategy(String),

    #[error("subpaths apply only to top-down paths; switch with `mt path strategy top-down`")]
    SubpathNotTopDown,

    #[error("a subpath needs at least one atom")]
    SubpathEmpty,

    #[error("subpath must end in a target atom; '{0}' is not a target of this path")]
    SubpathTailNotTarget(String),

    #[error("atom '{0}' appears more than once in the subpath")]
    SubpathDuplicateAtom(String),

    #[error("cards cache corrupt: {0}")]
    CardsCorrupt(String),

    #[error("TURSO_URL not set; pass --url")]
    MissingTursoUrl,

    #[error("turso: {0}")]
    Turso(String),

    #[error("{0} differences between the import and libSQL")]
    ImportMismatch(usize),

    #[error("flags {0} and {1} are mutually exclusive")]
    ConflictingFlags(&'static str, &'static str),

    #[error("missing MT_API_KEY or MT_ADMIN_PASSWORD")]
    MissingAuth,

    #[error("invalid bind address '{0}'")]
    BadBindAddr(String),

    #[error("invalid public URL '{0}'")]
    BadPublicUrl(String),

    #[error("bind {addr}: {source}")]
    Bind {
        addr: String,
        #[source]
        source: std::io::Error,
    },

    #[error("mcp server: {0}")]
    Serve(#[source] std::io::Error),
}
