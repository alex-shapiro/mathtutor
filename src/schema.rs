//! Database table schemas, row types, and initialization.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::db::client::Entry;
use crate::db::client::Expect;
use crate::db::client::api::{ColumnType, ErrorCode, ListTablesQuery, NewColumn, NewTable, Table};
use crate::db::{Op, Row, client, ts};
use crate::types::{Difficulty, QuizType, Rating, Strategy};
use crate::{Error, Result};

/// Key that records the schema version of the learner tables and the progress of `mt import-turso`.
pub(crate) const STATE_KEY: &str = "mathtutor_state";

/// Key that records the schema version of the authorization tables.
pub(crate) const AUTH_STATE_KEY: &str = "mathtutor_auth_state";

/// Group of tables that one schema version covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableSet {
    /// Learner history and overlay tables, which every command uses.
    Learner,
    /// OAuth tables, which only `mt mcp` uses.
    Auth,
}

impl TableSet {
    /// Return the schema version of this table set in this release.
    #[allow(
        clippy::match_same_arms,
        reason = "each set changes its version independently"
    )]
    pub fn version(self) -> &'static str {
        match self {
            TableSet::Learner => "3",
            TableSet::Auth => "3",
        }
    }

    /// Return the definition of every table in this set.
    pub fn tables(self) -> Vec<NewTable> {
        match self {
            TableSet::Learner => vec![
                PathFile::new_table(),
                LessonRow::new_table(),
                QuizRow::new_table(),
                RemovedQuizRow::new_table(),
                CardRecord::new_table(),
                TaughtRow::new_table(),
            ],
            TableSet::Auth => vec![
                OAuthClient::new_table(),
                OAuthCode::new_table(),
                OAuthAccessToken::new_table(),
                OAuthRefreshToken::new_table(),
                OAuthAddrCount::new_table(),
            ],
        }
    }

    /// Return the key that records the schema version of this set.
    fn state_key(self) -> &'static str {
        match self {
            TableSet::Learner => STATE_KEY,
            TableSet::Auth => AUTH_STATE_KEY,
        }
    }
}

impl std::fmt::Display for TableSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            TableSet::Learner => "learner",
            TableSet::Auth => "authorization",
        })
    }
}

/// Stored progress of `mt import-turso` on a database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportMarker {
    Open,
    Complete,
}

impl ImportMarker {
    /// Return the write operation that stores this marker with the learner schema version of this release.
    pub(crate) fn write_op(self) -> Op {
        state_write_op(TableSet::Learner, Some(self))
    }
}

/// Value stored at the state key of a table set.
#[derive(Serialize, Deserialize)]
struct StoredState {
    schema: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    import: Option<ImportMarker>,
}

/// Return the operation that stores the schema version of `set` in this release and `import`.
fn state_write_op(set: TableSet, import: Option<ImportMarker>) -> Op {
    let state = StoredState {
        schema: set.version().to_string(),
        import,
    };
    let value = serde_json::to_vec(&state).expect("a stored state serializes");
    Op::put(set.state_key(), value)
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
        #[serde(with = "ts")]
        pub expires_at: DateTime<Utc>,
    }
}

row_table! {
    /// Row in the `oauth_addr_counts` table: the events of one client address that one limit counts.
    pub struct OAuthAddrCount in "oauth_addr_counts", primary key (key) {
        /// Name of the limit and the client address, such as `failures/203.0.113.7`.
        pub key: String,
        pub n: u32,
        /// Time that the count started or last changed.
        #[serde(with = "ts")]
        pub since: DateTime<Utc>,
    }
}

/// Time after its last token grant at which the server deletes an OAuth client registration.
pub(crate) const OAUTH_CLIENT_TTL: std::time::Duration = std::time::Duration::from_hours(30 * 24);

/// Row of an OAuth table, which no learner command reads or writes.
#[cfg(feature = "mcp")]
pub(crate) trait OAuthRow: Row {}

#[cfg(feature = "mcp")]
impl OAuthRow for OAuthClient {}
#[cfg(feature = "mcp")]
impl OAuthRow for OAuthCode {}
#[cfg(feature = "mcp")]
impl OAuthRow for OAuthAccessToken {}
#[cfg(feature = "mcp")]
impl OAuthRow for OAuthRefreshToken {}
#[cfg(feature = "mcp")]
impl OAuthRow for OAuthAddrCount {}

/// Schema state of a table set in a database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SchemaState {
    /// The database does not exist.
    NoDatabase,
    /// The database exists without the tables of the set.
    NoTables,
    Current,
    /// The database stores a schema version that this release does not use.
    Other(String),
}

/// Schema state of a table set and import progress of a database, read from the state key of the set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DbState {
    pub schema: SchemaState,
    pub import: Option<ImportMarker>,
}

impl DbState {
    /// Decode the state of `set` from the entry stored at its state key in an existing database.
    pub(crate) fn from_entry(set: TableSet, entry: Option<&Entry>) -> Self {
        let Some(value) = entry.map(|e| e.value.data().cloned().unwrap_or_default()) else {
            return Self {
                schema: SchemaState::NoTables,
                import: None,
            };
        };
        match serde_json::from_slice::<StoredState>(&value) {
            Ok(stored) => Self {
                schema: if stored.schema == set.version() {
                    SchemaState::Current
                } else {
                    SchemaState::Other(stored.schema)
                },
                import: stored.import,
            },
            Err(_) => Self {
                schema: SchemaState::Other(String::from_utf8_lossy(&value).into_owned()),
                import: None,
            },
        }
    }
}

/// Return the operation that reads the state of `set` in a database.
pub(crate) fn state_op(set: TableSet) -> Op {
    Op::get(set.state_key())
}

/// Read the state of `set` in `db`.
pub(crate) async fn state(db: &client::Db, set: TableSet) -> Result<DbState> {
    match db.read_one(state_op(set)).await {
        Ok(r) => Ok(DbState::from_entry(set, r.entry()?.as_ref())),
        Err(e) if e.code() == Some(ErrorCode::NotFound) => Ok(DbState {
            schema: SchemaState::NoDatabase,
            import: None,
        }),
        Err(e) => Err(Error::from(e).into_not_applied()),
    }
}

/// Create every missing table of `set` and record the schema version of the set, unless the database already records a state for it.
///
/// Fails with [`Error::ForeignTable`] if the database holds a table that no table set defines, and with [`Error::TableMismatch`] if a table exists with other columns.
pub(crate) async fn create_tables(db: &client::Db, set: TableSet) -> Result<()> {
    refuse_foreign_tables(db).await?;
    for t in set.tables() {
        let stored = db.create_table(&t).await?.table;
        if !defines(&t, &stored) {
            return Err(Error::TableMismatch {
                db: db.name().to_string(),
                table: t.name,
            });
        }
    }
    let absent = Op::Check {
        key: set.state_key().into(),
        expect: Expect::Absent,
    };
    match db.write(&[absent, state_write_op(set, None)]).await {
        Err(e) if e.code() != Some(ErrorCode::PreconditionFailed) => Err(e.into()),
        _ => Ok(()),
    }
}

/// Fail with [`Error::ForeignTable`] if `db` holds a table that no table set defines, such as a table of another application.
async fn refuse_foreign_tables(db: &client::Db) -> Result<()> {
    let ours: Vec<String> = [TableSet::Learner, TableSet::Auth]
        .into_iter()
        .flat_map(TableSet::tables)
        .map(|t| t.name)
        .collect();
    let mut query = ListTablesQuery::default();
    loop {
        let page = db.list_tables(query.clone()).await?;
        if let Some(foreign) = page.tables.into_iter().find(|t| !ours.contains(&t.name)) {
            return Err(Error::ForeignTable {
                db: db.name().to_string(),
                table: foreign.name,
            });
        }
        match page.next_cursor {
            Some(cursor) => query.cursor = Some(cursor),
            None => return Ok(()),
        }
    }
}

/// Return `true` if `table` has the primary key and the columns of `def`.
fn defines(def: &NewTable, table: &Table) -> bool {
    let defined = def.columns.iter().map(|c| (&c.name, c.ty, c.nullable));
    let stored = table.columns.iter().map(|c| (&c.name, c.ty, c.nullable));
    table.primary_key == def.primary_key && defined.eq(stored)
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

    /// Return the FNV-1a hash of the JSON definition of `tables`.
    fn fingerprint(tables: &[NewTable]) -> String {
        let json = serde_json::to_vec(tables).unwrap();
        let hash = json.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
        });
        format!("{hash:016x}")
    }

    #[test]
    fn each_table_set_has_the_version_of_its_definition() {
        let pinned = |set: TableSet| (set.version(), fingerprint(&set.tables()));
        let hint = "a table changed: raise the version of its set, then pin the new fingerprint";
        assert_eq!(
            pinned(TableSet::Learner),
            ("3", "da597d822afa7638".into()),
            "{hint}"
        );
        assert_eq!(
            pinned(TableSet::Auth),
            ("3", "09479194933c807a".into()),
            "{hint}"
        );
    }
}
