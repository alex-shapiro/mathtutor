//! Baserunna database handle and configuration.

pub mod ts;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use baserunna_client::{self as client, Op, OpResult, Row, Transport};

use client::api::ErrorCode;

use crate::graph::Curriculum;
#[cfg(feature = "mcp")]
use crate::schema::OAuthRow;
use crate::schema::{self, DbState, ImportMarker, OAuthClient, SchemaState, TableSet};
use crate::view::{OverlayCache, OverlayWrite, WriteSet};
use crate::{Error, Result};

pub const DEFAULT_URL: &str = "https://baserunna.fly.dev";
pub const DEFAULT_DB: &str = "mathtutor";

#[derive(Clone)]
pub struct DbConfig {
    pub url: String,
    pub token: String,
    pub db: String,
    /// Directory used to persist the cached overlay across processes, or `None` for in-memory caching only.
    pub cache_dir: Option<PathBuf>,
    /// Curriculum that the user overlay amends.
    pub curriculum: Arc<Curriculum>,
}

impl std::fmt::Debug for DbConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbConfig")
            .field("url", &self.url)
            .field("token", &"<hidden>")
            .field("db", &self.db)
            .field("cache_dir", &self.cache_dir)
            .finish_non_exhaustive()
    }
}

/// Return the local Math Tutor home directory: `MATHTUTOR_HOME`, or `$HOME/.mathtutor`, or `None` if neither variable is set.
fn mt_home() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("MATHTUTOR_HOME") {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".mathtutor"))
}

impl DbConfig {
    /// Read `BASERUNNA_URL`, `BASERUNNA_TOKEN`, and `BASERUNNA_DB` from the environment, and use the embedded curriculum.
    pub fn from_env() -> Result<Self> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Ok(Self {
            url: var("BASERUNNA_URL").unwrap_or_else(|| DEFAULT_URL.into()),
            token: var("BASERUNNA_TOKEN").ok_or(Error::MissingToken)?,
            db: var("BASERUNNA_DB").unwrap_or_else(|| DEFAULT_DB.into()),
            cache_dir: mt_home(),
            curriculum: Curriculum::embedded(),
        })
    }

    /// Return the HTTP transport to the configured server.
    pub fn transport(&self) -> Result<client::HttpTransport> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("mathtutor/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_mins(1))
            .pool_idle_timeout(Duration::from_secs(50))
            .build()?;
        Ok(client::HttpTransport::with_client(&self.url, http))
    }
}

/// Handle to the learner database, with the overlay cached across transactions.
#[derive(Clone)]
pub struct Db {
    client: client::Db,
    url: Arc<str>,
    pub(crate) overlay_cache: Arc<Mutex<OverlayCache>>,
    pub(crate) cache_file: Option<PathBuf>,
    pub(crate) curriculum: Arc<Curriculum>,
    allow_open_import: bool,
    /// Each request also checks the schema version of the authorization tables.
    checks_auth_tables: bool,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db")
            .field("name", &self.client.name())
            .finish_non_exhaustive()
    }
}

impl Db {
    /// Create the database and its tables if they do not exist, and return `true` if this call created the database.
    pub async fn create_if_missing(&self) -> Result<bool> {
        self.create_tables_if_missing()
            .await
            .map_err(|e| self.setup_error(e))
    }

    /// Return the error of a failed table setup, which did not apply the command's writes.
    fn setup_error(&self, e: Error) -> Error {
        self.explain_key_error(e).into_not_applied()
    }

    async fn create_tables_if_missing(&self) -> Result<bool> {
        let state = schema::state(&self.client, TableSet::Learner).await?.schema;
        let created = state == SchemaState::NoDatabase;
        match state {
            SchemaState::Current => {}
            SchemaState::Other(_) => return Err(self.schema_error(TableSet::Learner, state)),
            SchemaState::NoDatabase => {
                self.client
                    .ensure()
                    .await
                    .map_err(|e| self.creation_error(e))?;
                schema::create_tables(&self.client, TableSet::Learner).await?;
            }
            SchemaState::NoTables => schema::create_tables(&self.client, TableSet::Learner).await?,
        }
        Ok(created)
    }

    /// Create the tables of each checked table set that the existing database does not hold, and replace the tables of each set that an older release stored.
    async fn set_up_tables(&self) -> Result<()> {
        let setup = async {
            for &set in self.checked_sets() {
                match schema::state(&self.client, set).await?.schema {
                    SchemaState::NoTables => schema::create_tables(&self.client, set).await?,
                    SchemaState::Other(found) if set.replaces(&found) => {
                        tracing::warn!(tables = %set, %found, "replacing tables of an older schema version");
                        schema::recreate_tables(&self.client, set).await?;
                    }
                    _ => {}
                }
            }
            Ok(())
        };
        setup.await.map_err(|e| self.setup_error(e))
    }

    /// Return a handle whose requests also check the authorization tables, and create them in the first request if they are missing.
    #[must_use]
    pub fn with_auth_tables(&self) -> Self {
        Self {
            checks_auth_tables: true,
            ..self.clone()
        }
    }

    /// Return `true` if [`Self::set_up_tables`] repairs the database state that `e` reports.
    fn needs_table_setup(e: &Error) -> bool {
        match e {
            Error::TablesMissing { .. } => true,
            Error::SchemaMismatch { tables, found, .. } => tables.replaces(found),
            _ => false,
        }
    }

    /// Return the error that reports `state` of `set` for a database that is not ready.
    fn schema_error(&self, set: TableSet, state: SchemaState) -> Error {
        let db = self.name().to_string();
        match (set, state) {
            (_, SchemaState::Other(found)) => Error::SchemaMismatch {
                db,
                tables: set,
                found,
                expected: set.version(),
            },
            (_, SchemaState::NoTables) => Error::TablesMissing { db, tables: set },
            _ => Error::DatabaseNotFound {
                db,
                url: self.url.to_string(),
            },
        }
    }

    /// Return the name of this database.
    pub fn name(&self) -> &str {
        self.client.name()
    }

    /// Return the curriculum that the user overlay of this database amends.
    pub fn curriculum(&self) -> &Arc<Curriculum> {
        &self.curriculum
    }

    /// Return the URL of the Baserunna server.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Return a handle that can use the database while an import is open.
    pub(crate) fn for_import(&self) -> Self {
        Self {
            allow_open_import: true,
            ..self.clone()
        }
    }

    /// Return a handle that reads the overlay from the database instead of a cache.
    #[must_use]
    pub fn without_overlay_cache(&self) -> Self {
        Self {
            overlay_cache: Arc::default(),
            cache_file: None,
            ..self.clone()
        }
    }

    /// Return every table set whose state each request of this handle checks.
    fn checked_sets(&self) -> &'static [TableSet] {
        if self.checks_auth_tables {
            &[TableSet::Learner, TableSet::Auth]
        } else {
            &[TableSet::Learner]
        }
    }

    /// Fail unless `state` of `set` permits this handle to use the database.
    fn check_state(&self, set: TableSet, state: DbState) -> Result<()> {
        if state.schema != SchemaState::Current {
            return Err(self.schema_error(set, state.schema));
        }
        if state.import == Some(ImportMarker::Open) && !self.allow_open_import {
            return Err(Error::ImportInProgress);
        }
        Ok(())
    }

    /// Read and check the state of every checked table set.
    async fn check_states(&self) -> Result<()> {
        for &set in self.checked_sets() {
            self.check_state(set, schema::state(&self.client, set).await?)?;
        }
        Ok(())
    }

    /// Return `ops` after the reads of the table set states that each request checks.
    fn gated(&self, ops: &[Op]) -> Vec<Op> {
        let sets = self.checked_sets().iter().map(|&set| schema::state_op(set));
        sets.chain(ops.iter().cloned()).collect()
    }

    /// Check the states that a request read before `ops`, and return the learner import marker and the results of `ops`.
    fn pass_gate(
        &self,
        mut results: Vec<OpResult>,
    ) -> Result<(Option<ImportMarker>, Vec<OpResult>)> {
        let sets = self.checked_sets();
        if results.len() < sets.len() {
            return Err(Error::BadRow("request did not return a state".into()));
        }
        let rest = results.split_off(sets.len());
        let mut import = None;
        for (&set, result) in sets.iter().zip(results) {
            let entry = result.entry()?;
            let state = DbState::from_entry(set, entry.as_ref());
            if set == TableSet::Learner {
                import = state.import;
            }
            self.check_state(set, state)?;
        }
        Ok((import, rest))
    }

    /// Return the error for a failed request, and report a missing database or a blocked state instead of a missing table or stream.
    async fn gate_failure(&self, e: client::Error) -> Error {
        if e.code() != Some(ErrorCode::NotFound) {
            return self.explain_key_error(e.into());
        }
        match self.check_states().await {
            Ok(()) => e.into(),
            Err(check) => self.explain_key_error(check),
        }
    }

    /// Return the configuration error that `e` reports if the server rejected or limited the key, or `e` otherwise.
    fn explain_key_error(&self, e: Error) -> Error {
        match &e {
            Error::Baserunna(c) => self.key_error(c).unwrap_or(e),
            _ => e,
        }
    }

    /// Return [`Error::TokenRejected`] or [`Error::KeyDenied`] if `e` reports that the server rejected or limited the key.
    fn key_error(&self, e: &client::Error) -> Option<Error> {
        match e.code()? {
            ErrorCode::Unauthenticated => Some(Error::TokenRejected {
                url: self.url.to_string(),
            }),
            ErrorCode::PermissionDenied => Some(Error::KeyDenied {
                db: self.name().to_string(),
                url: self.url.to_string(),
                reason: api_message(e),
            }),
            _ => None,
        }
    }

    /// Return the error for a failed creation of this database.
    fn creation_error(&self, e: client::Error) -> Error {
        if e.code() == Some(ErrorCode::PermissionDenied) {
            return Error::KeyCannotCreate {
                db: self.name().to_string(),
                url: self.url.to_string(),
                reason: api_message(&e),
            };
        }
        e.into()
    }

    /// Read `ops` at the latest version after the database and import checks, and set up the tables first if necessary.
    pub(crate) async fn read(&self, ops: &[Op]) -> Result<Vec<OpResult>> {
        match self.read_once(ops).await {
            Err(e) if Self::needs_table_setup(&e) => {
                self.set_up_tables().await?;
                self.read_once(ops).await
            }
            result => result,
        }
    }

    async fn read_once(&self, ops: &[Op]) -> Result<Vec<OpResult>> {
        match self.client.read(&self.gated(ops)).await {
            Ok(results) => self.pass_gate(results).map(|(_, rest)| rest),
            Err(e) => Err(self.gate_failure(e).await.into_not_applied()),
        }
    }

    /// Fail unless the database exists and the first request of this handle can use it, without a change to the database.
    ///
    /// Tables that the next request creates or replaces count as usable.
    pub async fn check_usable(&self) -> Result<()> {
        match self.read_once(&[]).await {
            Err(e) if Self::needs_table_setup(&e) => Ok(()),
            result => result.map(|_| ()),
        }
    }

    /// Read `op` at the latest version after the database and import checks.
    pub(crate) async fn read_one(&self, op: Op) -> Result<OpResult> {
        let mut results = self.read(std::slice::from_ref(&op)).await?;
        results
            .pop()
            .ok_or_else(|| Error::BadRow("read did not return a result".into()))
    }

    /// Read `op` and every next page of its result at the latest version, without a transaction.
    pub(crate) async fn read_pages(&self, mut op: Op) -> Result<Vec<OpResult>> {
        let mut pages = Vec::new();
        loop {
            let page = self.read_one(op.clone()).await?;
            let next = op.next_page(&page);
            pages.push(page);
            match next {
                Some(n) => op = n,
                None => return Ok(pages),
            }
        }
    }

    /// Read every page of stream `stream` after the database and import checks, or an empty list if the stream does not exist.
    pub(crate) async fn read_stream(&self, stream: String) -> Result<Vec<OpResult>> {
        match self.read_pages(Op::read_stream(stream, 0)).await {
            Err(Error::Baserunna(e)) if e.code() == Some(ErrorCode::NotFound) => Ok(Vec::new()),
            pages => pages,
        }
    }

    /// Insert OAuth row `row`, which the server deletes after `ttl`.
    #[cfg(feature = "mcp")]
    pub(crate) async fn insert_oauth<R: OAuthRow>(&self, row: &R, ttl: Duration) -> Result<()> {
        self.commit(&[Op::insert_expiring(row, ttl)]).await
    }

    /// Delete the OAuth row of type `R` with primary key `pk`.
    #[cfg(feature = "mcp")]
    pub(crate) async fn delete_oauth<R: OAuthRow>(&self, pk: &str) -> Result<()> {
        self.commit(&[Op::delete_row::<R>(pk)]).await
    }

    /// Store `clients` in a single transaction without an expiry, replacing a stored client with the same ID.
    pub(crate) async fn replace_oauth_clients(&self, clients: &[OAuthClient]) -> Result<()> {
        let ops: Vec<Op> = clients
            .iter()
            .flat_map(|c| [Op::delete_row::<OAuthClient>(&c.client_id), Op::insert(c)])
            .collect();
        self.commit(&ops).await
    }

    /// Record `marker` as the progress of an import into this database.
    pub(crate) async fn write_import_marker(&self, marker: ImportMarker) -> Result<()> {
        self.commit(&[marker.write_op()]).await
    }

    /// Commit `ops` after the database and import checks.
    async fn commit(&self, ops: &[Op]) -> Result<()> {
        run_txn!(self, |txn| {
            txn.queue(ops.iter().cloned());
            Ok::<_, Error>(())
        })
    }

    /// Begin a transaction whose first request runs the database and import checks.
    pub(crate) fn begin(&self) -> Txn {
        Txn {
            inner: self.client.begin(),
            checked: false,
            ran_table_setup: false,
            import: None,
            db: self.clone(),
        }
    }

    /// Return the retry state of a [`run_txn`] loop.
    pub(crate) fn retry(&self) -> Retry {
        Retry {
            inner: self.client.retry(),
            ran_table_setup: false,
        }
    }

    /// Create a handle without network I/O. The first request fails with [`Error::DatabaseNotFound`] if the database does not exist.
    pub fn connect(cfg: &DbConfig) -> Result<Self> {
        Ok(Self::with_transport(cfg, cfg.transport()?))
    }

    /// Create a client handle that sends requests through `transport` instead of HTTP to `cfg.url`.
    pub fn with_transport(cfg: &DbConfig, transport: impl Transport + 'static) -> Self {
        let cache_file = cfg
            .cache_dir
            .as_ref()
            .map(|d| d.join(cache_name(cfg.url.trim_end_matches('/'), &cfg.db)));
        let client = client::Client::new(transport).with_token(&cfg.token);
        Self {
            client: client.db(&cfg.db),
            url: Arc::from(cfg.url.as_str()),
            overlay_cache: Arc::default(),
            cache_file,
            curriculum: cfg.curriculum.clone(),
            allow_open_import: false,
            checks_auth_tables: false,
        }
    }
}

/// Run `$attempt` with `$txn` bound to a new transaction of `$db` and commit its writes, restarting it after a conflict.
///
/// Evaluates to the result of the committed attempt. A macro lets `$attempt` borrow local state mutably inside a `Send` future, which an async closure parameter cannot do.
macro_rules! run_txn {
    ($db:expr, |$txn:ident| $attempt:expr) => {{
        let db: &$crate::db::Db = $db;
        let mut retry = db.retry();
        loop {
            let mut $txn = db.begin();
            let result = $attempt;
            match retry.finish($txn, result).await {
                Ok(Some(out)) => break Ok(out),
                Ok(None) => {}
                Err(e) => break Err(e),
            }
        }
    }};
}
pub(crate) use run_txn;

/// Retry state of a [`run_txn`] loop.
pub(crate) struct Retry {
    inner: client::Retry,
    /// An attempt of this loop set up missing or outdated tables.
    ran_table_setup: bool,
}

impl Retry {
    /// Commit `txn` if `result` is `Ok`, or abort it, and return `None` if the attempt must restart.
    ///
    /// An attempt restarts once after its transaction set up missing or outdated tables.
    pub(crate) async fn finish<T, E>(
        &mut self,
        mut txn: Txn,
        result: std::result::Result<T, E>,
    ) -> std::result::Result<Option<T>, E>
    where
        E: client::TxnError + From<Error>,
    {
        let result = match result {
            Ok(out) => txn.check_gate().await.map(|()| out).map_err(E::from),
            Err(e) => Err(e),
        };
        let restart = txn.ran_table_setup && !self.ran_table_setup;
        self.ran_table_setup |= txn.ran_table_setup;
        let db = txn.db.clone();
        match self.inner.finish(txn.inner, result).await {
            Err(_) if restart => Ok(None),
            done => done.map_err(|e| match e.client_error().and_then(|c| db.key_error(c)) {
                Some(key_error) => E::from(key_error),
                None => e,
            }),
        }
    }
}

/// Serializable transaction whose first request runs the database and import checks.
pub(crate) struct Txn {
    inner: client::Txn,
    /// A request of this transaction ran the database and import checks.
    checked: bool,
    /// The checks of this transaction found missing or outdated tables and set them up.
    ran_table_setup: bool,
    /// Import marker that the checks of this transaction read.
    import: Option<ImportMarker>,
    db: Db,
}

impl Txn {
    /// Execute `ops` in the transaction.
    pub(crate) async fn run(&mut self, ops: &[Op]) -> Result<Vec<OpResult>> {
        if self.checked {
            return self
                .inner
                .run(ops)
                .await
                .map_err(|e| self.db.explain_key_error(e.into()));
        }
        self.checked = true;
        let result = match self.inner.run(&self.db.gated(ops)).await {
            Ok(results) => self.db.pass_gate(results).map(|(import, rest)| {
                self.import = import;
                rest
            }),
            Err(e) => Err(self.db.gate_failure(e).await),
        };
        if let Err(e) = &result
            && Db::needs_table_setup(e)
        {
            self.db.set_up_tables().await?;
            self.ran_table_setup = true;
        }
        result
    }

    /// Run the database and import checks unless an earlier request of this transaction already ran them.
    async fn check_gate(&mut self) -> Result<()> {
        if !self.checked {
            self.run(&[]).await?;
        }
        Ok(())
    }

    /// Read the OAuth row of type `R` with primary key `pk` in the transaction.
    #[cfg(feature = "mcp")]
    pub(crate) async fn get_oauth<R: OAuthRow>(&mut self, pk: &str) -> Result<Option<R>> {
        let mut results = self.run(&[Op::get_row::<R>(pk)]).await?;
        let result = results
            .pop()
            .ok_or_else(|| Error::BadRow("transaction did not return a result".into()))?;
        Ok(result.row()?)
    }

    /// Queue the insert of OAuth row `row`, which the server deletes after `ttl`.
    #[cfg(feature = "mcp")]
    pub(crate) fn insert_oauth<R: OAuthRow>(&mut self, row: &R, ttl: Duration) {
        self.queue([Op::insert_expiring(row, ttl)]);
    }

    /// Queue the insert of OAuth row `row`, which the server keeps until a delete.
    #[cfg(feature = "mcp")]
    pub(crate) fn insert_lasting_oauth<R: OAuthRow>(&mut self, row: &R) {
        self.queue([Op::insert(row)]);
    }

    /// Queue the delete of the OAuth row of type `R` with primary key `pk`.
    #[cfg(feature = "mcp")]
    pub(crate) fn delete_oauth<R: OAuthRow>(&mut self, pk: &str) {
        self.queue([Op::delete_row::<R>(pk)]);
    }

    /// Return the import marker that the database and import checks of the first request of this transaction read.
    pub(crate) fn import_marker(&self) -> Option<ImportMarker> {
        self.import
    }

    /// Queue `marker` as the progress of an import into this database.
    pub(crate) fn set_import_marker(&mut self, marker: ImportMarker) {
        self.queue([marker.write_op()]);
    }

    /// Queue the writes of `changes`, and return its overlay edits if it has any.
    pub(crate) fn queue_write_set(&mut self, write_set: WriteSet) -> Option<OverlayWrite> {
        let (ops, overlay_write) = write_set.into_ops();
        self.queue(ops);
        overlay_write
    }

    /// Queue write operations to send with the next request or the commit.
    fn queue(&mut self, ops: impl IntoIterator<Item = Op>) {
        self.inner.write(ops);
    }
}

/// Return the message of the API error in `e`.
fn api_message(e: &client::Error) -> String {
    match e {
        client::Error::Api { error, .. } => error.message.clone(),
        client::Error::NotApplied(inner) => api_message(inner),
        other => other.to_string(),
    }
}

fn cache_name(url: &str, db: &str) -> String {
    let host: String = url
        .split("://")
        .last()
        .unwrap_or(url)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("overlay-{host}-{db}.json")
}
