//! Baserunna database handle and configuration.

pub mod ts;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use baserunna_client::{self as client, Op, OpResult, Row, Transport};

use crate::schema::{self, SchemaState};
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
}

impl DbConfig {
    /// Read `BASERUNNA_URL`, `BASERUNNA_TOKEN`, and `BASERUNNA_DB` from the environment.
    pub fn from_env() -> Result<Self> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Ok(Self {
            url: var("BASERUNNA_URL").unwrap_or_else(|| DEFAULT_URL.into()),
            token: var("BASERUNNA_TOKEN").ok_or(Error::MissingToken)?,
            db: var("BASERUNNA_DB").unwrap_or_else(|| DEFAULT_DB.into()),
            cache_dir: crate::path::mt_home().ok(),
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
    pub(crate) client: client::Db,
    url: Arc<str>,
    pub(crate) overlay_cache: Arc<Mutex<Option<crate::view::CachedOverlay>>>,
    pub(crate) cache_file: Option<PathBuf>,
    pub(crate) allow_open_import: bool,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db")
            .field("name", &self.client.name())
            .finish_non_exhaustive()
    }
}

impl Db {
    /// Connect to an existing database, or fail with [`Error::DatabaseNotFound`] if it does not exist.
    pub async fn open(cfg: &DbConfig) -> Result<Self> {
        let db = Self::connect(cfg)?;
        match schema::state(&db.client).await? {
            SchemaState::Current => Ok(db),
            other => Err(db.schema_error(other)),
        }
    }

    /// Create the database and its tables if they do not exist, and return `true` if this call created the database.
    pub async fn create_if_missing(&self) -> Result<bool> {
        let state = schema::state(&self.client).await?;
        let created = state == SchemaState::NoDatabase;
        match state {
            SchemaState::Current => return Ok(false),
            SchemaState::Other(_) => return Err(self.schema_error(state)),
            SchemaState::NoDatabase => self.client.ensure().await?,
            SchemaState::NoTables => {}
        }
        schema::create_tables(&self.client).await?;
        Ok(created)
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

    /// Read `ops` at the latest version.
    pub(crate) async fn read(&self, ops: &[Op]) -> Result<Vec<OpResult>> {
        self.client
            .read(ops)
            .await
            .map_err(|e| Error::from(e).into_not_applied())
    }

    /// Read `op` at the latest version.
    pub(crate) async fn read_one(&self, op: Op) -> Result<OpResult> {
        self.client
            .read_one(op)
            .await
            .map_err(|e| Error::from(e).into_not_applied())
    }

    /// Create a client handle without performing network I/O; the database and tables must already exist.
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
            allow_open_import: false,
        }
    }

    /// Return the Baserunna client of this database.
    pub fn client(&self) -> &client::Client {
        self.client.client()
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
