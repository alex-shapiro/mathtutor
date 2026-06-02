//! Shared helpers for integration tests.

use libsql::{Connection, params};
use mathtutor::db::{self, DbConfig};
use tempfile::TempDir;

/// Open a freshly-migrated libSQL database under `dir` and seed one
/// path row keyed by `path_id`. Callers add their own `path_targets`,
/// events, etc. on top.
pub async fn fresh_db(dir: &TempDir, path_id: &str) -> Connection {
    let cfg = DbConfig::local(dir.path().join("mt.db"));
    let database = db::open(&cfg).await.expect("open");
    let conn = db::connect(&database).await.expect("connect");
    conn.execute(
        "INSERT INTO paths(id, goal, created_at) VALUES (?, ?, ?)",
        params![path_id, "test goal", "2026-05-26T00:00:00Z"],
    )
    .await
    .expect("seed path");
    conn
}
