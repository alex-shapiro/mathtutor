//! libSQL database setup and schema migration.
//!
//! All per-user state is stored in one libSQL database at
//! `$MATHTUTOR_HOME/mt.db` (default `~/.mathtutor/mt.db`). The db
//! opens as a synced local replica if  `TURSO_URL` and `TURSO_AUTH_TOKEN`
//! env variables are both set. If either is unset, it falls back to a
//! local-only file.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use libsql::{Builder, Connection, Database, params};

use crate::path::mt_home;
use crate::{Error, Result};

/// Max wall-clock time we'll spend on a foreground sync to the Turso
/// replica. Beyond this we warn and let libSQL catch up on a later call.
const SYNC_TIMEOUT: Duration = Duration::from_secs(10);

/// Canonical timestamp format for every `DATETIME` column we write.
/// Matches `SQLite`'s own `strftime('%Y-%m-%dT%H:%M:%fZ', 'now')` so the
/// built-in date functions can still operate on these strings, and the
/// `Z` suffix keeps lexicographic ordering aligned with chronological
/// ordering (`to_rfc3339`'s `+00:00` would break that the moment a
/// future migration ever wrote a non-UTC timestamp).
pub fn format_ts(ts: DateTime<Utc>) -> String {
    ts.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Parse a timestamp string read out of a `DATETIME` column. Accepts
/// any RFC 3339 form, which covers both the canonical `format_ts`
/// output and any legacy `to_rfc3339` strings already in the DB.
pub fn parse_ts(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| Error::BadTimestamp(format!("{s}: {e}")))
}

/// Numbered schema migration. Versions are strictly increasing,
/// starting at 1. Each migration file is immutable once shipped.
struct Migration {
    version: u32,
    name: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "init",
        sql: include_str!("migrations/001_init.sql"),
    },
    Migration {
        version: 2,
        name: "oauth",
        sql: include_str!("migrations/002_oauth.sql"),
    },
];

/// Schema migrations bookkeeping table
const META_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS schema_migrations (
        version    INTEGER PRIMARY KEY,
        name       TEXT NOT NULL,
        applied_at DATETIME NOT NULL
    );
";

/// libSQL credentials for syncing a local replica to server
#[derive(Debug, Clone)]
pub struct SyncConfig {
    pub url: String,
    pub auth_token: String,
}

impl SyncConfig {
    /// Constructs a config from explicit values.
    /// Returns `None` if either field is empty.
    pub fn new(url: String, auth_token: String) -> Option<Self> {
        if url.is_empty() || auth_token.is_empty() {
            return None;
        }
        Some(Self { url, auth_token })
    }

    /// Constructs a config from `TURSO_URL` and `TURSO_AUTH_TOKEN` env variables.
    /// Returns `None` if either is unset or empty.
    pub fn from_env() -> Option<Self> {
        let url = std::env::var("TURSO_URL").ok()?;
        let auth_token = std::env::var("TURSO_AUTH_TOKEN").ok()?;
        Self::new(url, auth_token)
    }
}

/// Resolved configuration for opening the user database.
#[derive(Debug, Clone)]
pub struct DbConfig {
    pub local_path: PathBuf,
    pub sync: Option<SyncConfig>,
}

impl DbConfig {
    /// Production config: local path under `mt_home()`, sync settings
    /// from `TURSO_URL` / `TURSO_AUTH_TOKEN`.
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            local_path: default_path()?,
            sync: SyncConfig::from_env(),
        })
    }

    /// Test/CLI-injectable config: explicit local path, no sync.
    pub fn local<P: Into<PathBuf>>(local_path: P) -> Self {
        Self {
            local_path: local_path.into(),
            sync: None,
        }
    }
}

/// Default on-disk location: `$MATHTUTOR_HOME/mt.db`.
pub fn default_path() -> Result<PathBuf> {
    Ok(mt_home()?.join("mt.db"))
}

/// Open the user database, applying any pending schema migrations.
/// Creates the parent directory if missing. Idempotent: safe to call
/// on a fresh directory or against an already-initialized file.
pub async fn open(cfg: &DbConfig) -> Result<Database> {
    if let Some(parent) = cfg.local_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }

    let db = if let Some(sync) = &cfg.sync {
        Builder::new_synced_database(&cfg.local_path, sync.url.clone(), sync.auth_token.clone())
            .build()
            .await?
    } else {
        Builder::new_local(&cfg.local_path).build().await?
    };

    let conn = connect(&db).await?;
    migrate(&conn).await?;
    Ok(db)
}

/// Open a new db connection to with FK enforcement enabled.
pub async fn connect(db: &Database) -> Result<Connection> {
    let conn = db.connect()?;
    conn.execute_batch("PRAGMA foreign_keys = ON;").await?;
    Ok(conn)
}

/// Apply all new migrations
pub async fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(META_SCHEMA).await?;
    let current = current_version(conn).await?;
    for m in MIGRATIONS {
        if m.version <= current {
            continue;
        }
        apply_migration(conn, m).await?;
    }
    Ok(())
}

async fn current_version(conn: &Connection) -> Result<u32> {
    let mut rows = conn
        .query(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            params![],
        )
        .await?;
    // SQLite aggregates without GROUP BY always return exactly one row,
    // and the version PK is bounded by the migration count we ship.
    let row = rows.next().await?.expect("aggregate returns one row");
    let version: i64 = row.get(0)?;
    Ok(u32::try_from(version).expect("schema version fits in u32"))
}

async fn apply_migration(conn: &Connection, m: &Migration) -> Result<()> {
    let tx = conn.transaction().await?;
    tx.execute_batch(m.sql).await?;
    tx.execute(
        "INSERT INTO schema_migrations(version, name, applied_at) \
         VALUES (?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        params![m.version, m.name],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Convenience for binaries: open with the env-derived config.
pub async fn open_default() -> Result<Database> {
    open(&DbConfig::from_env()?).await
}

/// Push local writes to the remote Turso replica if sync is configured.
/// No-op for local-only databases. On timeout (after `SYNC_TIMEOUT`) or
/// transport failure we warn to stderr and return — libSQL will retry
/// on the next sync, so a transient hiccup never fails a CLI command.
pub async fn maybe_sync(db: &Database, cfg: &DbConfig) {
    if cfg.sync.is_none() {
        return;
    }
    match tokio::time::timeout(SYNC_TIMEOUT, db.sync()).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => eprintln!("warning: turso sync failed: {e}"),
        Err(_) => eprintln!(
            "warning: turso sync timed out after {}s",
            SYNC_TIMEOUT.as_secs()
        ),
    }
}

/// Test-only helper: open a local database at `path` with no sync.
#[doc(hidden)]
pub async fn open_local(path: &Path) -> Result<Database> {
    open(&DbConfig::local(path.to_path_buf())).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_config_requires_both_fields_non_empty() {
        let cfg =
            SyncConfig::new("libsql://example.turso.io".into(), "secret".into()).expect("both set");
        assert_eq!(cfg.url, "libsql://example.turso.io");
        assert_eq!(cfg.auth_token, "secret");

        assert!(SyncConfig::new(String::new(), "secret".into()).is_none());
        assert!(SyncConfig::new("libsql://x".into(), String::new()).is_none());
        assert!(SyncConfig::new(String::new(), String::new()).is_none());
    }

    #[test]
    fn db_config_local_has_no_sync() {
        let cfg = DbConfig::local("/tmp/never-opened.db");
        assert!(cfg.sync.is_none());
        assert_eq!(cfg.local_path, PathBuf::from("/tmp/never-opened.db"));
    }
}
