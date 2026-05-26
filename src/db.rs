//! libSQL database setup and schema migration.
//!
//! The `mt` server keeps all per-user state in a single `SQLite` database
//! at `$MATHTUTOR_HOME/mt.db` (default `~/.mathtutor/mt.db`). When the
//! `TURSO_URL` and `TURSO_AUTH_TOKEN` environment variables are both set,
//! the database is opened as a libSQL embedded replica with offline
//! writes; otherwise it falls back to a plain local `SQLite` file. The
//! same `Database` handle covers both modes — callers don't need to
//! branch on transport.

use std::path::{Path, PathBuf};

use libsql::{Builder, Connection, Database};

use crate::path::mt_home;
use crate::{Error, Result};

const SCHEMA: &str = include_str!("schema.sql");

/// libsql credentials for syncing a local replica to server
#[derive(Debug, Clone)]
pub struct SyncConfig {
    pub url: String,
    pub auth_token: String,
}

impl SyncConfig {
    /// Read `TURSO_URL` and `TURSO_AUTH_TOKEN` from the process
    /// environment. Returns `None` if either is missing or empty.
    pub fn from_env() -> Option<Self> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Build from an arbitrary lookup. Factored out so tests can drive
    /// the same logic from a `HashMap` without touching process env.
    fn from_lookup<F: Fn(&str) -> Option<String>>(get: F) -> Option<Self> {
        let url = get("TURSO_URL").filter(|s| !s.is_empty())?;
        let auth_token = get("TURSO_AUTH_TOKEN").filter(|s| !s.is_empty())?;
        Some(Self { url, auth_token })
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
            local_path: default_db_path()?,
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
pub fn default_db_path() -> Result<PathBuf> {
    Ok(mt_home()?.join("mt.db"))
}

/// Open the user database, applying the schema migration. Creates the
/// parent directory if missing. Idempotent: safe to call on a fresh
/// directory or against an already-initialized file.
pub async fn open(cfg: &DbConfig) -> Result<Database> {
    if let Some(parent) = cfg.local_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }

    let db = match &cfg.sync {
        Some(sync) => {
            Builder::new_synced_database(&cfg.local_path, sync.url.clone(), sync.auth_token.clone())
                .build()
                .await?
        }
        None => Builder::new_local(&cfg.local_path).build().await?,
    };

    let conn = db.connect()?;
    migrate(&conn).await?;
    Ok(db)
}

/// Apply the schema migration on `conn`. All statements use
/// `IF NOT EXISTS`, so a second call is a no-op.
pub async fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA).await?;
    Ok(())
}

/// Convenience for binaries: open with the env-derived config.
pub async fn open_default() -> Result<Database> {
    open(&DbConfig::from_env()?).await
}

/// Test-only helper: open a local database at `path` with no sync.
#[doc(hidden)]
pub async fn open_local(path: &Path) -> Result<Database> {
    open(&DbConfig::local(path.to_path_buf())).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    #[test]
    fn sync_config_both_set() {
        let cfg = SyncConfig::from_lookup(lookup(&[
            ("TURSO_URL", "libsql://example.turso.io"),
            ("TURSO_AUTH_TOKEN", "secret"),
        ]))
        .expect("sync config should be present");
        assert_eq!(cfg.url, "libsql://example.turso.io");
        assert_eq!(cfg.auth_token, "secret");
    }

    #[test]
    fn sync_config_missing_token() {
        assert!(
            SyncConfig::from_lookup(lookup(&[("TURSO_URL", "libsql://example.turso.io")]))
                .is_none()
        );
    }

    #[test]
    fn sync_config_missing_url() {
        assert!(SyncConfig::from_lookup(lookup(&[("TURSO_AUTH_TOKEN", "secret")])).is_none());
    }

    #[test]
    fn sync_config_empty_strings_treated_as_unset() {
        assert!(
            SyncConfig::from_lookup(lookup(&[("TURSO_URL", ""), ("TURSO_AUTH_TOKEN", "x")]))
                .is_none()
        );
        assert!(
            SyncConfig::from_lookup(lookup(&[("TURSO_URL", "x"), ("TURSO_AUTH_TOKEN", "")]))
                .is_none()
        );
    }

    #[test]
    fn db_config_local_has_no_sync() {
        let cfg = DbConfig::local("/tmp/never-opened.db");
        assert!(cfg.sync.is_none());
        assert_eq!(cfg.local_path, PathBuf::from("/tmp/never-opened.db"));
    }
}
