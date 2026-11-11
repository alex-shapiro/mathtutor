//! Database table schemas, row types, and initialization.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::db::client::Entry;
use crate::db::client::api::{ColumnType, ErrorCode, NewColumn, NewTable};
use crate::db::{Op, Row, client, ts};
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

/// Column type and nullability of a row field type.
trait ColumnValue {
    const TYPE: ColumnType;
    const NULLABLE: bool = false;
}

impl ColumnValue for String {
    const TYPE: ColumnType = ColumnType::String;
}

impl ColumnValue for DateTime<Utc> {
    const TYPE: ColumnType = ColumnType::String;
}

impl ColumnValue for Difficulty {
    const TYPE: ColumnType = ColumnType::String;
}

impl ColumnValue for QuizType {
    const TYPE: ColumnType = ColumnType::String;
}

impl ColumnValue for Strategy {
    const TYPE: ColumnType = ColumnType::String;
}

impl ColumnValue for u32 {
    const TYPE: ColumnType = ColumnType::Int;
}

impl ColumnValue for f32 {
    const TYPE: ColumnType = ColumnType::Float;
}

impl<T> ColumnValue for Vec<T> {
    const TYPE: ColumnType = ColumnType::Json;
}

impl<T: ColumnValue> ColumnValue for Option<T> {
    const TYPE: ColumnType = T::TYPE;
    const NULLABLE: bool = true;
}

fn column<T: ColumnValue>(name: &str) -> NewColumn {
    if T::NULLABLE {
        NewColumn::nullable(name, T::TYPE)
    } else {
        NewColumn::new(name, T::TYPE)
    }
}

/// Row type with a table definition.
trait TableRow: Row {
    fn new_table() -> NewTable;
}

/// Declare a row struct, its table name, and its table definition from a single field list.
///
/// Each field becomes a column with the type that [`ColumnValue`] gives for the field type.
macro_rules! row_table {
    (
        $(#[$attr:meta])*
        pub struct $row:ident in $table:literal, primary key ($($key:ident),+) {
            $(
                $(#[$field_attr:meta])*
                pub $field:ident: $ty:ty,
            )+
        }
    ) => {
        $(#[$attr])*
        #[derive(Debug, Clone, Serialize, Deserialize)]
        pub struct $row {
            $(
                $(#[$field_attr])*
                pub $field: $ty,
            )+
        }

        impl Row for $row {
            const TABLE: &'static str = $table;
        }

        impl TableRow for $row {
            fn new_table() -> NewTable {
                let _primary_key_fields_exist = |r: &$row| {
                    $(let _ = &r.$key;)+
                };
                NewTable {
                    name: $table.into(),
                    columns: vec![$(column::<$ty>(stringify!($field))),+],
                    primary_key: vec![$(stringify!($key).to_string()),+],
                    indexes: Vec::new(),
                    if_not_exists: true,
                }
            }
        }
    };
}

row_table! {
    /// Stored metadata and configuration for a learning path.
    pub struct PathFile in "paths", primary key (id) {
        pub id: String,
        pub goal: String,
        #[serde(with = "ts")]
        pub created_at: DateTime<Utc>,
        pub targets: Vec<String>,
        pub strategy: Strategy,
        pub subpath: Vec<String>,
    }
}

row_table! {
    /// Row in the `lessons` overlay table.
    pub struct LessonRow in "lessons", primary key (atom_id) {
        pub atom_id: String,
        pub body: String,
    }
}

row_table! {
    /// Row in the `quizzes` overlay table.
    pub struct QuizRow in "quizzes", primary key (quiz_id) {
        pub quiz_id: String,
        pub atom_id: String,
        pub difficulty: Difficulty,
        pub kind: Option<QuizType>,
        pub question: String,
        pub answer: String,
        pub rubric: Option<String>,
    }
}

row_table! {
    /// Row in the `removed_quizzes` overlay table.
    pub struct RemovedQuizRow in "removed_quizzes", primary key (quiz_id) {
        pub quiz_id: String,
        pub atom_id: String,
    }
}

row_table! {
    /// Row in the `cards` table storing FSRS state and answer history for a quiz.
    pub struct CardRecord in "cards", primary key (path_id, quiz_id) {
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
}

row_table! {
    /// Row in the `taught` table tracking lesson presentations per path.
    pub struct TaughtRow in "taught", primary key (path_id, atom_id) {
        pub path_id: String,
        pub atom_id: String,
        pub count: u32,
        #[serde(with = "ts::option")]
        pub last_at: Option<DateTime<Utc>>,
    }
}

row_table! {
    /// Row in the `oauth_clients` table.
    pub struct OAuthClient in "oauth_clients", primary key (client_id) {
        pub client_id: String,
        pub client_name: Option<String>,
        pub redirect_uris: Vec<String>,
        #[serde(with = "ts")]
        pub created_at: DateTime<Utc>,
    }
}

row_table! {
    /// Row in the `oauth_codes` table.
    pub struct OAuthCode in "oauth_codes", primary key (code_hash) {
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
    }
}

row_table! {
    /// Row in the `oauth_access_tokens` table.
    pub struct OAuthAccessToken in "oauth_access_tokens", primary key (token_hash) {
        /// Hex SHA-256 digest of the access token.
        pub token_hash: String,
        pub client_id: String,
        pub scope: Option<String>,
        #[serde(with = "ts")]
        pub expires_at: DateTime<Utc>,
    }
}

row_table! {
    /// Row in the `oauth_refresh_tokens` table.
    pub struct OAuthRefreshToken in "oauth_refresh_tokens", primary key (token_hash) {
        /// Hex SHA-256 digest of the refresh token.
        pub token_hash: String,
        pub client_id: String,
        pub scope: Option<String>,
        #[serde(with = "ts")]
        pub expires_at: DateTime<Utc>,
    }
}

/// Return the schema definition for all tables.
pub fn tables() -> Vec<NewTable> {
    vec![
        PathFile::new_table(),
        LessonRow::new_table(),
        QuizRow::new_table(),
        RemovedQuizRow::new_table(),
        CardRecord::new_table(),
        TaughtRow::new_table(),
        OAuthClient::new_table(),
        OAuthCode::new_table(),
        OAuthAccessToken::new_table(),
        OAuthRefreshToken::new_table(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_follow_the_field_types_of_their_row() {
        let cards = CardRecord::new_table();
        let column = |name: &str| {
            let c = cards.columns.iter().find(|c| c.name == name).unwrap();
            (c.ty, c.nullable)
        };
        assert_eq!(cards.name, "cards");
        assert_eq!(cards.primary_key, ["path_id", "quiz_id"]);
        assert_eq!(column("path_id"), (ColumnType::String, false));
        assert_eq!(column("stability"), (ColumnType::Float, true));
        assert_eq!(column("due_at"), (ColumnType::String, true));
        assert_eq!(column("reps"), (ColumnType::Int, false));
        assert_eq!(column("recent"), (ColumnType::Json, false));
        assert_eq!(QuizRow::new_table().columns[2].ty, ColumnType::String);
    }

    #[test]
    fn serialized_fields_of_a_row_match_its_columns() {
        let row = TaughtRow {
            path_id: "p".into(),
            atom_id: "a".into(),
            count: 1,
            last_at: None,
        };
        let mut fields: Vec<String> = match serde_json::to_value(&row).unwrap() {
            serde_json::Value::Object(m) => m.keys().cloned().collect(),
            other => panic!("{other}"),
        };
        let mut columns: Vec<String> = TaughtRow::new_table()
            .columns
            .into_iter()
            .map(|c| c.name)
            .collect();
        columns.sort();
        fields.sort();
        assert_eq!(columns, fields);
    }
}
