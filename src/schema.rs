//! Database table schemas, row types, and initialization.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::db::client::Entry;
use crate::db::client::api::{ColumnType, ErrorCode, NewColumn, NewTable};
use crate::db::{Op, Row, client, ts};
use crate::path::PathFile;
use crate::types::{Difficulty, QuizType, Rating, Strategy};
use crate::{Error, Result};

pub(crate) const VERSION: &str = "2";
const VERSION_KEY: &str = "schema_version";

/// Key that records the progress of `mt import-turso` on this database.
pub(crate) const IMPORT_STATE_KEY: &str = "turso_import_state";

/// Stored progress of `mt import-turso` on a database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMarker {
    Open,
    Complete,
}

impl ImportMarker {
    const OPEN_VALUE: &'static str = "running";
    const COMPLETE_VALUE: &'static str = "complete";

    /// Decode the marker stored at [`IMPORT_STATE_KEY`].
    pub(crate) fn from_entry(entry: Option<Entry>) -> Result<Option<Self>> {
        let Some(entry) = entry else {
            return Ok(None);
        };
        match entry.value.data().map(AsRef::as_ref) {
            Some(v) if v == Self::OPEN_VALUE.as_bytes() => Ok(Some(Self::Open)),
            Some(v) if v == Self::COMPLETE_VALUE.as_bytes() => Ok(Some(Self::Complete)),
            _ => Err(Error::BadRow(format!("{IMPORT_STATE_KEY}: unknown value"))),
        }
    }

    /// Return the write operation that stores this marker.
    pub(crate) fn write_op(self) -> Op {
        let value = match self {
            Self::Open => Self::OPEN_VALUE,
            Self::Complete => Self::COMPLETE_VALUE,
        };
        Op::put(IMPORT_STATE_KEY, value)
    }
}

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
pub(crate) struct PathUpdate<'a> {
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
    /// Hex SHA-256 digest of the authorization code.
    pub code_hash: String,
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
    /// Hex SHA-256 digest of the access token.
    pub token_hash: String,
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
    /// Hex SHA-256 digest of the refresh token.
    pub token_hash: String,
    pub client_id: String,
    pub scope: Option<String>,
    #[serde(with = "ts")]
    pub expires_at: DateTime<Utc>,
}

impl Row for OAuthRefreshToken {
    const TABLE: &'static str = "oauth_refresh_tokens";
}

fn table(name: &str, primary_key: &[&str], columns: Vec<NewColumn>) -> NewTable {
    NewTable {
        name: name.into(),
        columns,
        primary_key: primary_key.iter().map(|c| (*c).to_string()).collect(),
        indexes: Vec::new(),
        if_not_exists: true,
    }
}

fn string(name: &str) -> NewColumn {
    NewColumn::new(name, ColumnType::String)
}

fn string_or_null(name: &str) -> NewColumn {
    NewColumn::nullable(name, ColumnType::String)
}

fn int(name: &str) -> NewColumn {
    NewColumn::new(name, ColumnType::Int)
}

fn float_or_null(name: &str) -> NewColumn {
    NewColumn::nullable(name, ColumnType::Float)
}

fn json(name: &str) -> NewColumn {
    NewColumn::new(name, ColumnType::Json)
}

fn token_columns() -> Vec<NewColumn> {
    vec![
        string("token_hash"),
        string("client_id"),
        string_or_null("scope"),
        string("expires_at"),
    ]
}

/// Return the schema definition for all tables.
pub fn tables() -> Vec<NewTable> {
    vec![
        table(
            PathFile::TABLE,
            &["id"],
            vec![
                string("id"),
                string("goal"),
                string("created_at"),
                string("strategy"),
                json("targets"),
                json("subpath"),
            ],
        ),
        table(
            LessonRow::TABLE,
            &["atom_id"],
            vec![string("atom_id"), string("body")],
        ),
        table(
            QuizRow::TABLE,
            &["quiz_id"],
            vec![
                string("quiz_id"),
                string("atom_id"),
                string("difficulty"),
                string_or_null("kind"),
                string("question"),
                string("answer"),
                string_or_null("rubric"),
            ],
        ),
        table(
            RemovedQuizRow::TABLE,
            &["quiz_id"],
            vec![string("quiz_id"), string("atom_id")],
        ),
        table(
            CardRecord::TABLE,
            &["path_id", "quiz_id"],
            vec![
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
        ),
        table(
            TaughtRow::TABLE,
            &["path_id", "atom_id"],
            vec![
                string("path_id"),
                string("atom_id"),
                int("count"),
                string_or_null("last_at"),
            ],
        ),
        table(
            OAuthClient::TABLE,
            &["client_id"],
            vec![
                string("client_id"),
                string_or_null("client_name"),
                json("redirect_uris"),
                string("created_at"),
            ],
        ),
        table(
            OAuthCode::TABLE,
            &["code_hash"],
            vec![
                string("code_hash"),
                string("client_id"),
                string("redirect_uri"),
                string_or_null("scope"),
                string("code_challenge"),
                string("code_challenge_method"),
                string_or_null("resource"),
                string("expires_at"),
                string_or_null("used_at"),
            ],
        ),
        table(OAuthAccessToken::TABLE, &["token_hash"], token_columns()),
        table(OAuthRefreshToken::TABLE, &["token_hash"], token_columns()),
    ]
}

/// Schema state of a database, read from [`VERSION_KEY`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SchemaState {
    /// The database does not exist.
    NoDatabase,
    /// The database exists without Math Tutor tables.
    NoTables,
    Current,
    /// The database stores a schema version that this release does not use.
    Other(String),
}

impl SchemaState {
    /// Decode the schema state from the entry stored at [`VERSION_KEY`].
    pub(crate) fn from_entry(entry: Option<&Entry>) -> Self {
        match entry.and_then(|e| e.value.data()) {
            None => Self::NoTables,
            Some(v) if v.as_ref() == VERSION.as_bytes() => Self::Current,
            Some(v) => Self::Other(String::from_utf8_lossy(v).into_owned()),
        }
    }
}

/// Return the operation that reads the schema version.
pub(crate) fn version_op() -> Op {
    Op::get(VERSION_KEY)
}

/// Read the schema state of `db`.
pub(crate) async fn state(db: &client::Db) -> Result<SchemaState> {
    match db.read_one(version_op()).await {
        Ok(r) => Ok(SchemaState::from_entry(r.entry()?.as_ref())),
        Err(e) if e.code() == Some(ErrorCode::NotFound) => Ok(SchemaState::NoDatabase),
        Err(e) => Err(Error::from(e).into_not_applied()),
    }
}

/// Create every table and record the schema version.
pub(crate) async fn create_tables(db: &client::Db) -> Result<()> {
    for t in tables() {
        db.create_table(&t).await?;
    }
    db.write(&[Op::put(VERSION_KEY, VERSION)]).await?;
    Ok(())
}
