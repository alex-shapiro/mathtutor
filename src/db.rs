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
    Migration {
        version: 3,
        name: "strategy_subpath",
        sql: include_str!("migrations/003_strategy_subpath.sql"),
    },
    Migration {
        version: 4,
        name: "rename_target_column",
        sql: include_str!("migrations/004_rename_target_column.sql"),
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
///
/// If sync is configured and the local path holds a plain `SQLite` file
/// from an earlier non-sync session, the file is upgraded in place to
/// a libSQL embedded replica without losing data.
pub async fn open(cfg: &DbConfig) -> Result<Database> {
    if let Some(parent) = cfg.local_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::FileIo {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }

    let db = if let Some(sync) = &cfg.sync {
        if needs_replica_upgrade(&cfg.local_path) {
            return upgrade_local_to_replica(&cfg.local_path, sync).await;
        }
        let db = Builder::new_synced_database(
            &cfg.local_path,
            sync.url.clone(),
            sync.auth_token.clone(),
        )
        .build()
        .await?;
        // Pull remote frames before local writes, including migrations.
        // Otherwise we see a version conflict during sync.
        db.sync().await?;
        db
    } else {
        Builder::new_local(&cfg.local_path).build().await?
    };

    let conn = connect(&db).await?;
    migrate(&conn).await?;
    Ok(db)
}

/// libSQL's embedded replica stores its frame-tracking metadata in a
/// sibling file named `<path>-info`. Its presence is what tells us a
/// local file is already a replica (versus a plain `SQLite` file left by
/// an earlier non-sync session).
fn info_sidecar(local_path: &Path) -> PathBuf {
    let mut name = local_path.file_name().unwrap_or_default().to_os_string();
    name.push("-info");
    local_path.with_file_name(name)
}

/// `true` when there's a plain local `SQLite` file at `local_path` but no
/// replica metadata. This can occur if the user previously ran `mt` without
/// `TURSO_*` set and is now turning sync on.
fn needs_replica_upgrade(local_path: &Path) -> bool {
    local_path.exists() && !info_sidecar(local_path).exists()
}

/// Convert a plain local `SQLite` file into an embedded replica without
/// losing data. The old file is renamed to `<path>.preupgrade` and
/// preserved; a fresh replica is opened at the original path, migrated
/// to the current schema, populated from the backup via `ATTACH`, and
/// synced to push the rows up to Turso.
///
/// Bails out if `<path>.preupgrade` already exists. The user has already
/// upgraded, and a silently overwrite will destroy whatever the user
/// was trying to recover.
async fn upgrade_local_to_replica(local_path: &Path, sync: &SyncConfig) -> Result<Database> {
    let backup_path = {
        let mut name = local_path.file_name().unwrap_or_default().to_os_string();
        name.push(".preupgrade");
        local_path.with_file_name(name)
    };
    if backup_path.exists() {
        return Err(Error::FileIo {
            path: backup_path,
            source: std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "previous upgrade backup exists; resolve manually before retrying",
            ),
        });
    }

    tracing::info!(
        backup = %backup_path.display(),
        "converting local SQLite file to libSQL embedded replica",
    );

    std::fs::rename(local_path, &backup_path).map_err(|e| Error::FileIo {
        path: local_path.to_path_buf(),
        source: e,
    })?;

    let db = Builder::new_synced_database(local_path, sync.url.clone(), sync.auth_token.clone())
        .build()
        .await?;

    let conn = connect(&db).await?;
    migrate(&conn).await?;
    copy_user_tables_via_attach(&conn, &backup_path).await?;
    drop(conn);

    // Push the freshly-imported rows to Turso so the local replica and
    // the remote agree before the caller starts reading.
    let cfg = DbConfig {
        local_path: local_path.to_path_buf(),
        sync: Some(sync.clone()),
    };
    maybe_sync(&db, &cfg).await;

    tracing::info!(
        backup = %backup_path.display(),
        "replica upgrade complete; original file preserved",
    );
    Ok(db)
}

/// Copy every user table from the `SQLite` file at `backup_path` into the
/// connection's `main` schema using `ATTACH`. Skips `schema_migrations`
/// (already populated by [`migrate`] in the new replica) and any table
/// names that don't exist in the backup. `INSERT OR IGNORE` covers the
/// edge case where the replica's initial sync from remote already
/// brought matching rows back.
async fn copy_user_tables_via_attach(conn: &Connection, backup_path: &Path) -> Result<()> {
    let attach_path = backup_path.to_str().ok_or_else(|| Error::FileIo {
        path: backup_path.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "backup path is not valid UTF-8",
        ),
    })?;
    conn.execute("ATTACH DATABASE ? AS legacy", params![attach_path])
        .await?;

    // The data is internally consistent (it came from a working DB), so
    // turning FK enforcement off during the copy lets us insert in any
    // order without weakening any invariant.
    conn.execute_batch("PRAGMA foreign_keys = OFF;").await?;
    for table in list_user_tables(conn, "legacy").await? {
        if table == "schema_migrations" {
            continue;
        }
        let sql = format!(r#"INSERT OR IGNORE INTO main."{table}" SELECT * FROM legacy."{table}""#);
        conn.execute(&sql, ()).await?;
    }
    conn.execute_batch("PRAGMA foreign_keys = ON;").await?;
    conn.execute_batch("DETACH DATABASE legacy;").await?;
    Ok(())
}

async fn list_user_tables(conn: &Connection, schema: &str) -> Result<Vec<String>> {
    let sql = format!(
        "SELECT name FROM {schema}.sqlite_master \
         WHERE type='table' AND name NOT LIKE 'sqlite_%'"
    );
    let mut rows = conn.query(&sql, ()).await?;
    let mut tables = Vec::new();
    while let Some(row) = rows.next().await? {
        tables.push(row.get::<String>(0)?);
    }
    Ok(tables)
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
/// transport failure we warn to stderr and return. libSQL will retry
/// on the next sync, so a transient hiccup never fails a CLI command.
pub async fn maybe_sync(db: &Database, cfg: &DbConfig) {
    if cfg.sync.is_none() {
        return;
    }
    match tokio::time::timeout(SYNC_TIMEOUT, db.sync()).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "turso sync failed"),
        Err(_) => tracing::warn!(
            timeout_secs = SYNC_TIMEOUT.as_secs(),
            "turso sync timed out",
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
    use tempfile::TempDir;

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

    #[test]
    fn info_sidecar_appends_dash_info() {
        assert_eq!(
            info_sidecar(Path::new("/var/lib/mt.db")),
            PathBuf::from("/var/lib/mt.db-info"),
        );
        assert_eq!(
            info_sidecar(Path::new("mt.db")),
            PathBuf::from("mt.db-info")
        );
    }

    #[test]
    fn needs_replica_upgrade_detects_plain_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("mt.db");

        assert!(!needs_replica_upgrade(&path), "missing file => no upgrade");

        std::fs::write(&path, b"plain sqlite bytes").unwrap();
        assert!(needs_replica_upgrade(&path), "plain file => upgrade");

        std::fs::write(info_sidecar(&path), b"replica meta").unwrap();
        assert!(
            !needs_replica_upgrade(&path),
            "-info sidecar present => already a replica"
        );
    }

    /// End-to-end check of the data-preservation step: a plain local DB
    /// with user rows is renamed to a backup, a fresh DB is opened at
    /// the original path and migrated, and `copy_user_tables_via_attach`
    /// pulls every row across. This is the part of the upgrade that
    /// must not lose data; the surrounding sync plumbing needs a real
    /// Turso endpoint and is not exercised here.
    #[tokio::test]
    async fn copy_user_tables_via_attach_preserves_rows() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("mt.db");

        // 1. Plain local DB with one row in `paths` and one in `events`.
        let old = Builder::new_local(&path).build().await.unwrap();
        let conn = connect(&old).await.unwrap();
        migrate(&conn).await.unwrap();
        conn.execute(
            "INSERT INTO paths(id, goal, created_at) VALUES (?, ?, ?)",
            params!["p1", "learn calc", "2026-05-31T00:00:00Z"],
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO events(ts, kind, path_id, payload) VALUES (?, ?, ?, ?)",
            params!["2026-05-31T00:00:01Z", "path_created", "p1", r#"{"k":"v"}"#],
        )
        .await
        .unwrap();
        drop(conn);
        drop(old);

        // 2. Simulate the rename step of `upgrade_local_to_replica`.
        let backup = path.with_extension("db.preupgrade");
        std::fs::rename(&path, &backup).unwrap();

        // 3. Fresh DB at the original path, migrated to current schema.
        let fresh = Builder::new_local(&path).build().await.unwrap();
        let conn = connect(&fresh).await.unwrap();
        migrate(&conn).await.unwrap();

        // 4. Copy from backup.
        copy_user_tables_via_attach(&conn, &backup).await.unwrap();

        // 5. Rows are present in the fresh DB.
        let mut rows = conn
            .query("SELECT goal FROM paths WHERE id = ?", params!["p1"])
            .await
            .unwrap();
        let row = rows.next().await.unwrap().expect("path row");
        assert_eq!(row.get::<String>(0).unwrap(), "learn calc");

        let mut rows = conn
            .query(
                "SELECT payload FROM events WHERE path_id = ?",
                params!["p1"],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().expect("event row");
        assert_eq!(row.get::<String>(0).unwrap(), r#"{"k":"v"}"#);

        // FK enforcement is still on after the copy (the helper flips it
        // off mid-copy, then back on); inserting an orphan must fail.
        let bad = conn
            .execute(
                "INSERT INTO path_targets(path_id, target_id, position) VALUES (?, ?, ?)",
                params!["nope", "atom", 0],
            )
            .await;
        assert!(bad.is_err(), "FKs must be re-enabled after copy");
    }

    /// Copying into a fresh replica must be safe even when remote sync
    /// has already populated some rows: `INSERT OR IGNORE` skips PK
    /// collisions rather than failing the upgrade.
    #[tokio::test]
    async fn copy_user_tables_via_attach_tolerates_existing_rows() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("mt.db");

        let old = Builder::new_local(&path).build().await.unwrap();
        let conn = connect(&old).await.unwrap();
        migrate(&conn).await.unwrap();
        conn.execute(
            "INSERT INTO paths(id, goal, created_at) VALUES (?, ?, ?)",
            params!["dup", "old goal", "2026-05-31T00:00:00Z"],
        )
        .await
        .unwrap();
        drop(conn);
        drop(old);

        let backup = path.with_extension("db.preupgrade");
        std::fs::rename(&path, &backup).unwrap();

        let fresh = Builder::new_local(&path).build().await.unwrap();
        let conn = connect(&fresh).await.unwrap();
        migrate(&conn).await.unwrap();
        // Pre-existing remote row with the same PK as the backup.
        conn.execute(
            "INSERT INTO paths(id, goal, created_at) VALUES (?, ?, ?)",
            params!["dup", "remote goal", "2026-05-31T00:00:00Z"],
        )
        .await
        .unwrap();

        copy_user_tables_via_attach(&conn, &backup)
            .await
            .expect("copy must not fail on PK collisions");

        let mut rows = conn
            .query("SELECT goal FROM paths WHERE id = ?", params!["dup"])
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(
            row.get::<String>(0).unwrap(),
            "remote goal",
            "existing row wins; backup row is ignored"
        );
    }
}
