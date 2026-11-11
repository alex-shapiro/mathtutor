//! Baserunna database handle and configuration.

pub mod ts;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use baserunna_client::{self as client, Op, OpResult, Row, Transport};

use client::api::ErrorCode;

use crate::graph::Curriculum;
use crate::schema::{self, IMPORT_STATE_KEY, ImportMarker, SchemaState};
use crate::{Error, Result};

pub const DEFAULT_URL: &str = "https://baserunna.fly.dev";
pub const DEFAULT_DB: &str = "mathtutor";

#[derive(Debug, Clone)]
pub struct DbConfig {
    pub url: String,
    pub token: String,
    pub db: String,
    /// Directory used to persist the cached overlay across processes, or `None` for in-memory caching only.
    pub cache_dir: Option<PathBuf>,
    /// Curriculum that the user overlay amends.
    pub curriculum: Arc<Curriculum>,
}

impl DbConfig {
    /// Read `BASERUNNA_URL`, `BASERUNNA_TOKEN`, and `BASERUNNA_DB` from the environment, and use the embedded curriculum.
    pub fn from_env() -> Result<Self> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Ok(Self {
            url: var("BASERUNNA_URL").unwrap_or_else(|| DEFAULT_URL.into()),
            token: var("BASERUNNA_TOKEN").ok_or(Error::MissingToken)?,
            db: var("BASERUNNA_DB").unwrap_or_else(|| DEFAULT_DB.into()),
            cache_dir: crate::path::mt_home().ok(),
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
    pub(crate) overlay_cache: Arc<Mutex<Option<crate::view::CachedOverlay>>>,
    pub(crate) cache_file: Option<PathBuf>,
    pub(crate) curriculum: Arc<Curriculum>,
    allow_open_import: bool,
    schema_checked: Arc<AtomicBool>,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db")
            .field("name", &self.client.name())
            .finish_non_exhaustive()
    }
}

/// Checks that a request runs before its own operations.
struct Gate {
    /// Fail if an import into the database is open.
    import: bool,
    /// Fail if the database does not exist or has a different schema version.
    schema: bool,
}

impl Gate {
    fn ops(&self, ops: &[Op]) -> Vec<Op> {
        let schema = self.schema.then(schema::version_op);
        let import = self.import.then(|| Op::get(IMPORT_STATE_KEY));
        schema
            .into_iter()
            .chain(import)
            .chain(ops.iter().cloned())
            .collect()
    }

    fn len(&self) -> usize {
        usize::from(self.schema) + usize::from(self.import)
    }
}

impl Db {
    /// Create the database and its tables if they do not exist, and return `true` if this call created the database.
    pub async fn create_if_missing(&self) -> Result<bool> {
        let state = schema::state(&self.client).await?;
        let created = state == SchemaState::NoDatabase;
        match state {
            SchemaState::Current => {}
            SchemaState::Other(_) => return Err(self.schema_error(state)),
            SchemaState::NoDatabase => {
                self.client.ensure().await?;
                schema::create_tables(&self.client).await?;
            }
            SchemaState::NoTables => schema::create_tables(&self.client).await?,
        }
        self.schema_checked.store(true, Ordering::Relaxed);
        Ok(created)
    }

    /// Fail with [`Error::DatabaseNotFound`] or [`Error::SchemaMismatch`] unless the database holds the tables of this release.
    pub async fn require_database(&self) -> Result<()> {
        match schema::state(&self.client).await? {
            SchemaState::Current => {
                self.schema_checked.store(true, Ordering::Relaxed);
                Ok(())
            }
            other => Err(self.schema_error(other)),
        }
    }

    /// Return the error that reports `state` for a database that is not ready.
    fn schema_error(&self, state: SchemaState) -> Error {
        match state {
            SchemaState::Other(found) => Error::SchemaMismatch {
                db: self.name().to_string(),
                found,
                expected: schema::VERSION,
            },
            _ => Error::DatabaseNotFound {
                db: self.name().to_string(),
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
    pub(crate) fn without_overlay_cache(&self) -> Self {
        Self {
            overlay_cache: Arc::default(),
            cache_file: None,
            ..self.clone()
        }
    }

    fn gate(&self) -> Gate {
        Gate {
            import: !self.allow_open_import,
            schema: !self.schema_checked.load(Ordering::Relaxed),
        }
    }

    /// Check the results of `gate` at the front of `results` and return the remaining results.
    fn pass_gate(&self, gate: &Gate, mut results: Vec<OpResult>) -> Result<Vec<OpResult>> {
        let rest = results.split_off(gate.len().min(results.len()));
        let mut checks = results.into_iter();
        if gate.schema {
            let entry = checks.next().map(OpResult::entry).transpose()?.flatten();
            match SchemaState::from_entry(entry.as_ref()) {
                SchemaState::Current => self.schema_checked.store(true, Ordering::Relaxed),
                other => return Err(self.schema_error(other)),
            }
        }
        if gate.import {
            let entry = checks.next().map(OpResult::entry).transpose()?.flatten();
            if ImportMarker::from_entry(entry)? == Some(ImportMarker::Open) {
                return Err(Error::ImportInProgress);
            }
        }
        Ok(rest)
    }

    /// Return the error for a failed request, and report a missing database instead of a missing table or stream.
    async fn gate_failure(&self, gate: &Gate, e: client::Error) -> Error {
        if !gate.schema || e.code() != Some(ErrorCode::NotFound) {
            return e.into();
        }
        match schema::state(&self.client).await {
            Ok(SchemaState::Current) => {
                self.schema_checked.store(true, Ordering::Relaxed);
                e.into()
            }
            Ok(other) => self.schema_error(other),
            Err(check) => check,
        }
    }

    /// Read `ops` at the latest version after the database and import checks.
    pub(crate) async fn read(&self, ops: &[Op]) -> Result<Vec<OpResult>> {
        let gate = self.gate();
        let all = gate.ops(ops);
        if all.is_empty() {
            return Ok(Vec::new());
        }
        match self.client.read(&all).await {
            Ok(results) => self.pass_gate(&gate, results),
            Err(e) => Err(self.gate_failure(&gate, e).await.into_not_applied()),
        }
    }

    /// Read `op` at the latest version after the database and import checks.
    pub(crate) async fn read_one(&self, op: Op) -> Result<OpResult> {
        let mut results = self.read(std::slice::from_ref(&op)).await?;
        results
            .pop()
            .ok_or_else(|| Error::BadRow("read returned no result".into()))
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

    /// Commit `ops` after the database and import checks.
    pub(crate) async fn write(&self, ops: impl IntoIterator<Item = Op>) -> Result<()> {
        let ops: Vec<Op> = ops.into_iter().collect();
        run_txn!(self, |txn| {
            txn.write(ops.iter().cloned());
            Ok::<_, Error>(())
        })
    }

    /// Begin a transaction whose first request runs the database and import checks.
    pub(crate) fn begin(&self) -> Txn {
        Txn {
            inner: self.client.begin(),
            gate: Some(self.gate()),
            db: self.clone(),
        }
    }

    /// Return the retry state of a [`run_txn`] loop.
    pub(crate) fn retry(&self) -> Retry {
        Retry {
            inner: self.client.retry(),
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
        let cached = cache_file
            .as_deref()
            .and_then(crate::view::read_cached_overlay);
        let client = client::Client::new(transport).with_token(&cfg.token);
        Self {
            client: client.db(&cfg.db),
            url: Arc::from(cfg.url.as_str()),
            overlay_cache: Arc::new(Mutex::new(cached)),
            cache_file,
            curriculum: cfg.curriculum.clone(),
            allow_open_import: false,
            schema_checked: Arc::default(),
        }
    }

    /// Return the Baserunna client of this database.
    pub fn client(&self) -> &client::Client {
        self.client.client()
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
}

impl Retry {
    /// Commit `txn` if `result` is `Ok`, or abort it, and return `None` if the attempt must restart.
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
        self.inner.finish(txn.inner, result).await
    }
}

/// Serializable transaction whose first request runs the database and import checks.
pub(crate) struct Txn {
    inner: client::Txn,
    gate: Option<Gate>,
    db: Db,
}

impl Txn {
    /// Execute `ops` in the transaction.
    pub(crate) async fn run(&mut self, ops: &[Op]) -> Result<Vec<OpResult>> {
        let Some(gate) = self.gate.take() else {
            return Ok(self.inner.run(ops).await?);
        };
        match self.inner.run(&gate.ops(ops)).await {
            Ok(results) => self.db.pass_gate(&gate, results),
            Err(e) => Err(self.db.gate_failure(&gate, e).await),
        }
    }

    /// Run the database and import checks if no request of this transaction has run them.
    async fn check_gate(&mut self) -> Result<()> {
        if self.gate.is_some() {
            self.run(&[]).await?;
        }
        Ok(())
    }

    /// Execute `op` in the transaction.
    #[cfg(feature = "mcp")]
    pub(crate) async fn run_one(&mut self, op: Op) -> Result<OpResult> {
        let mut results = self.run(std::slice::from_ref(&op)).await?;
        results
            .pop()
            .ok_or_else(|| Error::BadRow("transaction returned no result".into()))
    }

    /// Queue write operations to send with the next request or the commit.
    pub(crate) fn write(&mut self, ops: impl IntoIterator<Item = Op>) {
        self.inner.write(ops);
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
