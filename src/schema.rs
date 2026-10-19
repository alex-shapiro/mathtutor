//! Database table schemas, row types, and initialization.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::Result;
use crate::db::{Db, Op, Row, ts};
use crate::error::Error;
use crate::path::PathFile;
use crate::types::{Difficulty, QuizType, Rating, Strategy};

const VERSION: u64 = 1;
const VERSION_KEY: &str = "schema_version";

/// Return the stream key for the event log of `path_id`.
pub fn events_stream(path_id: &str) -> String {
    format!("events.{path_id}")
}

/// Key prefix for per-path latest activity timestamps.
pub const ACTIVITY_PREFIX: &str = "activity/";

/// Return the key for the latest activity timestamp of `path_id`.
pub fn activity_key(path_id: &str) -> String {
    format!("{ACTIVITY_PREFIX}{path_id}")
}

impl Row for PathFile {
    const TABLE: &'static str = "paths";
}

/// Mutable columns of a `paths` row.
#[derive(Debug, Default, Serialize)]
pub struct PathUpdate<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<Strategy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subpath: Option<&'a [String]>,
}

/// Row in the `lessons` overlay table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LessonRow {
    pub atom_id: String,
    pub body: String,
}

impl Row for LessonRow {
    const TABLE: &'static str = "lessons";
}

/// Row in the `quizzes` overlay table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuizRow {
    pub quiz_id: String,
    pub atom_id: String,
    pub difficulty: Difficulty,
    pub kind: Option<QuizType>,
    pub question: String,
    pub answer: String,
    pub rubric: Option<String>,
}

impl Row for QuizRow {
    const TABLE: &'static str = "quizzes";
}

/// Row in the `removed_quizzes` overlay table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemovedQuizRow {
    pub quiz_id: String,
    pub atom_id: String,
}

impl Row for RemovedQuizRow {
    const TABLE: &'static str = "removed_quizzes";
}

/// Row in the `cards` table storing FSRS state and answer history for a quiz.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardRecord {
    pub path_id: String,
    pub quiz_id: String,
    pub stability: Option<f32>,
    pub difficulty: Option<f32>,
    #[serde(with = "ts::option")]
    pub due_at: Option<DateTime<Utc>>,
    #[serde(with = "ts::option")]
    pub last_reviewed_at: Option<DateTime<Utc>>,
    pub reps: u32,
    pub lapses: u32,
    pub presented: u32,
    #[serde(with = "ts::option")]
    pub last_presented_at: Option<DateTime<Utc>>,
    #[serde(with = "ts::option")]
    pub first_correct_at: Option<DateTime<Utc>>,
    /// Recent ratings ordered from newest to oldest.
    pub recent: Vec<Rating>,
}

impl Row for CardRecord {
    const TABLE: &'static str = "cards";
}

/// Row in the `taught` table tracking lesson presentations per path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaughtRow {
    pub path_id: String,
    pub atom_id: String,
    pub count: u32,
    #[serde(with = "ts::option")]
    pub last_at: Option<DateTime<Utc>>,
}

impl Row for TaughtRow {
    const TABLE: &'static str = "taught";
}

/// Row in the `oauth_clients` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthClient {
    pub client_id: String,
    pub client_name: Option<String>,
    pub redirect_uris: Vec<String>,
    #[serde(with = "ts")]
    pub created_at: DateTime<Utc>,
}

impl Row for OAuthClient {
    const TABLE: &'static str = "oauth_clients";
}

/// Row in the `oauth_codes` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthCode {
    pub code: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub scope: Option<String>,
    pub code_challenge: String,
    pub code_challenge_method: String,
    pub resource: Option<String>,
    #[serde(with = "ts")]
    pub expires_at: DateTime<Utc>,
    #[serde(with = "ts::option")]
    pub used_at: Option<DateTime<Utc>>,
}

impl Row for OAuthCode {
    const TABLE: &'static str = "oauth_codes";
}

/// Update payload marking an `oauth_codes` row as redeemed.
#[derive(Debug, Serialize)]
pub struct OAuthCodeUse {
    #[serde(with = "ts")]
    pub used_at: DateTime<Utc>,
}

/// Row in the `oauth_access_tokens` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthAccessToken {
    pub token: String,
    pub client_id: String,
    pub scope: Option<String>,
    #[serde(with = "ts")]
    pub expires_at: DateTime<Utc>,
}

impl Row for OAuthAccessToken {
    const TABLE: &'static str = "oauth_access_tokens";
}

/// Row in the `oauth_refresh_tokens` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthRefreshToken {
    pub token: String,
    pub client_id: String,
    pub scope: Option<String>,
    #[serde(with = "ts")]
    pub expires_at: DateTime<Utc>,
}

impl Row for OAuthRefreshToken {
    const TABLE: &'static str = "oauth_refresh_tokens";
}

#[derive(Serialize)]
struct Table {
    name: &'static str,
    primary_key: &'static [&'static str],
    columns: &'static [Column],
}

#[derive(Serialize)]
struct Column {
    name: &'static str,
    #[serde(rename = "type")]
    ty: ColumnType,
    nullable: bool,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum ColumnType {
    String,
    Int,
    Float,
    Json,
}

const fn col(name: &'static str, ty: ColumnType, nullable: bool) -> Column {
    Column { name, ty, nullable }
}

const fn string(name: &'static str) -> Column {
    col(name, ColumnType::String, false)
}

const fn string_or_null(name: &'static str) -> Column {
    col(name, ColumnType::String, true)
}

const fn int(name: &'static str) -> Column {
    col(name, ColumnType::Int, false)
}

const fn float_or_null(name: &'static str) -> Column {
    col(name, ColumnType::Float, true)
}

const fn json(name: &'static str) -> Column {
    col(name, ColumnType::Json, false)
}

const TABLES: &[Table] = &[
    Table {
        name: PathFile::TABLE,
        primary_key: &["id"],
        columns: &[
            string("id"),
            string("goal"),
            string("created_at"),
            string("strategy"),
            json("targets"),
            json("subpath"),
        ],
    },
    Table {
        name: LessonRow::TABLE,
        primary_key: &["atom_id"],
        columns: &[string("atom_id"), string("body")],
    },
    Table {
        name: QuizRow::TABLE,
        primary_key: &["quiz_id"],
        columns: &[
            string("quiz_id"),
            string("atom_id"),
            string("difficulty"),
            string_or_null("kind"),
            string("question"),
            string("answer"),
            string_or_null("rubric"),
        ],
    },
    Table {
        name: RemovedQuizRow::TABLE,
        primary_key: &["quiz_id"],
        columns: &[string("quiz_id"), string("atom_id")],
    },
    Table {
        name: CardRecord::TABLE,
        primary_key: &["path_id", "quiz_id"],
        columns: &[
            string("path_id"),
            string("quiz_id"),
            float_or_null("stability"),
            float_or_null("difficulty"),
            string_or_null("due_at"),
            string_or_null("last_reviewed_at"),
            int("reps"),
            int("lapses"),
            int("presented"),
            string_or_null("last_presented_at"),
            string_or_null("first_correct_at"),
            json("recent"),
        ],
    },
    Table {
        name: TaughtRow::TABLE,
        primary_key: &["path_id", "atom_id"],
        columns: &[
            string("path_id"),
            string("atom_id"),
            int("count"),
            string_or_null("last_at"),
        ],
    },
    Table {
        name: OAuthClient::TABLE,
        primary_key: &["client_id"],
        columns: &[
            string("client_id"),
            string_or_null("client_name"),
            json("redirect_uris"),
            string("created_at"),
        ],
    },
    Table {
        name: OAuthCode::TABLE,
        primary_key: &["code"],
        columns: &[
            string("code"),
            string("client_id"),
            string("redirect_uri"),
            string_or_null("scope"),
            string("code_challenge"),
            string("code_challenge_method"),
            string_or_null("resource"),
            string("expires_at"),
            string_or_null("used_at"),
        ],
    },
    Table {
        name: OAuthAccessToken::TABLE,
        primary_key: &["token"],
        columns: TOKEN_COLUMNS,
    },
    Table {
        name: OAuthRefreshToken::TABLE,
        primary_key: &["token"],
        columns: TOKEN_COLUMNS,
    },
];

const TOKEN_COLUMNS: &[Column] = &[
    string("token"),
    string("client_id"),
    string_or_null("scope"),
    string("expires_at"),
];

/// Ensure that the database and all required tables exist.
pub async fn ensure(db: &Db) -> Result<()> {
    match db.read(&[Op::get(VERSION_KEY)]).await {
        Ok(r) if r[0].value().and_then(|v| v.parse().ok()) == Some(VERSION) => return Ok(()),
        Ok(_) => {}
        Err(Error::Api(e)) if e.status == 404 => db.create_database().await?,
        Err(e) => return Err(e),
    }
    for t in TABLES {
        db.create_table(t).await?;
    }
    db.write(&[Op::put(VERSION_KEY, VERSION.to_string())]).await
}
