//! Baserunna database handle and configuration.

pub mod ts;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use baserunna_client::{self as client, Op, OpResult, Row, Transport};

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
    /// Reads `BASERUNNA_URL`, `BASERUNNA_TOKEN` (required), and `BASERUNNA_DB` from the environment.
    pub fn from_env() -> Result<Self> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Ok(Self {
            url: var("BASERUNNA_URL").unwrap_or_else(|| DEFAULT_URL.into()),
            token: var("BASERUNNA_TOKEN").ok_or(Error::MissingToken)?,
            db: var("BASERUNNA_DB").unwrap_or_else(|| DEFAULT_DB.into()),
            cache_dir: crate::path::mt_home().ok(),
        })
    }

    /// Returns the HTTP transport to the configured server.
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
    /// Connects to the database and creates the database and tables if they do not exist.
    pub async fn open(cfg: &DbConfig) -> Result<Self> {
        let db = Self::connect(cfg)?;
        crate::schema::ensure(&db.client)
            .await
            .map_err(Error::into_not_applied)?;
        Ok(db)
    }

    /// Returns a handle that can use the database while an import is open.
    pub(crate) fn for_import(&self) -> Self {
        Self {
            allow_open_import: true,
            ..self.clone()
        }
    }

    /// Reads `ops` at the latest version.
    pub(crate) async fn read(&self, ops: &[Op]) -> Result<Vec<OpResult>> {
        self.client
            .read(ops)
            .await
            .map_err(|e| Error::from(e).into_not_applied())
    }

    /// Reads `op` at the latest version.
    pub(crate) async fn read_one(&self, op: Op) -> Result<OpResult> {
        self.client
            .read_one(op)
            .await
            .map_err(|e| Error::from(e).into_not_applied())
    }

    /// Creates a client handle without performing network I/O; the database and tables must already exist.
    pub fn connect(cfg: &DbConfig) -> Result<Self> {
        Ok(Self::with_transport(cfg, cfg.transport()?))
    }

    /// Creates a client handle that sends requests through `transport` instead of HTTP to `cfg.url`.
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
            overlay_cache: Arc::new(Mutex::new(cached)),
            cache_file,
            allow_open_import: false,
        }
    }

    /// Returns the Baserunna client of this database.
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
