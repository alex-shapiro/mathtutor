//! Integration tests for the libSQL database foundation.
//!
//! Each test opens a fresh local database in a `tempdir` so runs don't
//! collide, then drives `db::open` / SQL directly to verify the
//! schema, idempotent migration, foreign-key enforcement, and the
//! `payload` JSON check constraint.

use std::path::PathBuf;

use libsql::{Builder, params};
use mathtutor::db::{self, DbConfig, SyncConfig};
use tempfile::TempDir;

fn cfg_in(dir: &TempDir) -> DbConfig {
    DbConfig::local(dir.path().join("mt.db"))
}

async fn collect_names(rows: &mut libsql::Rows) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.expect("next") {
        out.push(row.get::<String>(0).expect("name col"));
    }
    out.sort();
    out
}

#[tokio::test]
async fn open_creates_database_file() {
    let tmp = TempDir::new().unwrap();
    let cfg = cfg_in(&tmp);
    let _db = db::open(&cfg).await.expect("open");
    assert!(cfg.local_path.exists(), "db file should be created");
}

#[tokio::test]
async fn open_creates_nested_parent_dir() {
    let tmp = TempDir::new().unwrap();
    let nested = tmp.path().join("a").join("b").join("c");
    let cfg = DbConfig::local(nested.join("mt.db"));
    let _db = db::open(&cfg).await.expect("open");
    assert!(nested.exists());
}

#[tokio::test]
async fn schema_creates_all_expected_tables() {
    let tmp = TempDir::new().unwrap();
    let db = db::open(&cfg_in(&tmp)).await.unwrap();
    let conn = db::connect(&db).await.unwrap();

    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            params![],
        )
        .await
        .unwrap();
    let tables = collect_names(&mut rows).await;

    let expected: Vec<String> = [
        "cards",
        "events",
        "oauth_access_tokens",
        "oauth_authorization_codes",
        "oauth_clients",
        "oauth_refresh_tokens",
        "overlay_lessons",
        "overlay_quizzes",
        "overlay_removed_quizzes",
        "path_subpath",
        "path_targets",
        "paths",
        "schema_migrations",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    assert_eq!(tables, expected);
}

#[tokio::test]
async fn schema_creates_expected_indexes() {
    let tmp = TempDir::new().unwrap();
    let db = db::open(&cfg_in(&tmp)).await.unwrap();
    let conn = db::connect(&db).await.unwrap();

    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='index' AND name NOT LIKE 'sqlite_%'",
            params![],
        )
        .await
        .unwrap();
    let indexes = collect_names(&mut rows).await;

    let expected: Vec<String> = [
        "idx_cards_due",
        "idx_events_atom",
        "idx_events_path",
        "idx_events_quiz",
        "idx_oauth_access_tokens_exp",
        "idx_oauth_refresh_tokens_exp",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    assert_eq!(indexes, expected);
}

#[tokio::test]
async fn migration_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let cfg = cfg_in(&tmp);

    let db1 = db::open(&cfg).await.expect("first open");
    let conn = db::connect(&db1).await.unwrap();
    conn.execute(
        "INSERT INTO paths(id, goal, created_at) VALUES (?, ?, ?)",
        params!["p1", "learn", "2026-05-26T00:00:00Z"],
    )
    .await
    .unwrap();
    drop(conn);
    drop(db1);

    // Re-opening must not drop or corrupt data; the row persists.
    let db2 = db::open(&cfg).await.expect("second open");
    let conn = db::connect(&db2).await.unwrap();
    let mut rows = conn
        .query("SELECT goal FROM paths WHERE id = ?", params!["p1"])
        .await
        .unwrap();
    let row = rows.next().await.unwrap().expect("row");
    assert_eq!(row.get::<String>(0).unwrap(), "learn");
}

#[tokio::test]
async fn open_records_applied_migration() {
    let tmp = TempDir::new().unwrap();
    let db = db::open(&cfg_in(&tmp)).await.unwrap();
    let conn = db::connect(&db).await.unwrap();

    let mut rows = conn
        .query(
            "SELECT version, name FROM schema_migrations ORDER BY version",
            params![],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().expect("v1 row");
    assert_eq!(row.get::<i64>(0).unwrap(), 1);
    assert_eq!(row.get::<String>(1).unwrap(), "init");
    let row = rows.next().await.unwrap().expect("v2 row");
    assert_eq!(row.get::<i64>(0).unwrap(), 2);
    assert_eq!(row.get::<String>(1).unwrap(), "oauth");
    let row = rows.next().await.unwrap().expect("v3 row");
    assert_eq!(row.get::<i64>(0).unwrap(), 3);
    assert_eq!(row.get::<String>(1).unwrap(), "strategy_subpath");
    assert!(rows.next().await.unwrap().is_none(), "exactly three rows");
}

#[tokio::test]
async fn second_open_does_not_re_apply_migrations() {
    let tmp = TempDir::new().unwrap();
    let cfg = cfg_in(&tmp);

    let _db1 = db::open(&cfg).await.unwrap();
    let db2 = db::open(&cfg).await.unwrap();
    let conn = db::connect(&db2).await.unwrap();

    let mut rows = conn
        .query("SELECT COUNT(*) FROM schema_migrations", params![])
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(
        row.get::<i64>(0).unwrap(),
        3,
        "init + oauth + strategy_subpath migrations should each be recorded exactly once"
    );
}

#[tokio::test]
async fn foreign_keys_are_enforced() {
    let tmp = TempDir::new().unwrap();
    let db = db::open(&cfg_in(&tmp)).await.unwrap();
    let conn = db::connect(&db).await.unwrap();

    // path_targets.path_id references paths(id); the row should be
    // rejected because no parent exists.
    let res = conn
        .execute(
            "INSERT INTO path_targets(path_id, atom_id) VALUES (?, ?)",
            params!["does-not-exist", "tx.1.1"],
        )
        .await;
    assert!(
        res.is_err(),
        "FK violation should fail; db::connect must set PRAGMA foreign_keys = ON"
    );
}

#[tokio::test]
async fn payload_must_be_valid_json_or_null() {
    let tmp = TempDir::new().unwrap();
    let db = db::open(&cfg_in(&tmp)).await.unwrap();
    let conn = db::connect(&db).await.unwrap();

    conn.execute(
        "INSERT INTO paths(id, goal, created_at) VALUES (?, ?, ?)",
        params!["p1", "g", "2026-05-26T00:00:00Z"],
    )
    .await
    .unwrap();

    // NULL payload — accepted.
    conn.execute(
        "INSERT INTO events(ts, kind, path_id, payload) VALUES (?, ?, ?, NULL)",
        params!["2026-05-26T00:00:00Z", "path_created", "p1"],
    )
    .await
    .expect("null payload allowed");

    // Valid JSON — accepted.
    conn.execute(
        "INSERT INTO events(ts, kind, path_id, payload) VALUES (?, ?, ?, ?)",
        params![
            "2026-05-26T00:00:01Z",
            "quiz_answered",
            "p1",
            r#"{"rating":"good"}"#
        ],
    )
    .await
    .expect("valid json allowed");

    // Invalid JSON — rejected by the CHECK constraint.
    let bad = conn
        .execute(
            "INSERT INTO events(ts, kind, path_id, payload) VALUES (?, ?, ?, ?)",
            params![
                "2026-05-26T00:00:02Z",
                "quiz_answered",
                "p1",
                "not json at all"
            ],
        )
        .await;
    assert!(bad.is_err(), "invalid JSON payload should be rejected");
}

#[tokio::test]
async fn maybe_sync_is_noop_on_local_only_db() {
    // `maybe_sync` must short-circuit when `cfg.sync` is `None` — the
    // CLI calls it unconditionally after every mutating command, and a
    // local-only setup (no TURSO_URL) must finish silently rather than
    // hitting the libsql sync path and erroring on "not a synced db".
    let tmp = TempDir::new().unwrap();
    let cfg = cfg_in(&tmp);
    assert!(cfg.sync.is_none());
    let db = db::open(&cfg).await.expect("open");
    db::maybe_sync(&db, &cfg).await;
}

#[tokio::test]
async fn open_with_sync_pulls_remote_before_migrating() {
    // Regression: when sync is configured, `open` must pull remote
    // frames before `migrate` writes anything locally. A fresh replica
    // that runs `migrate` first creates a divergent commit generation;
    // the remote then rejects every push as `server returned a conflict`
    // (sent=1, got=N), and writes pile up on a fork that can never land.
    //
    // We pin that contract behaviorally: with sync configured against an
    // unreachable remote, `open` must fail rather than silently proceed
    // to migrate. Port 1 refuses connections, so the HTTP attempt
    // completes synchronously without hanging the test.
    let tmp = TempDir::new().unwrap();
    let local = tmp.path().join("mt.db");
    let cfg = DbConfig {
        local_path: local.clone(),
        sync: SyncConfig::new("http://127.0.0.1:1".into(), "irrelevant".into()),
    };
    assert!(cfg.sync.is_some(), "sync must be configured for this test");

    let err = db::open(&cfg)
        .await
        .expect_err("open must fail when the sync remote is unreachable");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("sync") || msg.contains("connect"),
        "expected a sync/connect error, got: {msg}",
    );

    // Migrate writes the bookkeeping table on its first run. If sync ran
    // before migrate (correct order), the table must not exist on disk
    // after the failed open. Reopening as a plain local db lets us look
    // without re-running migrations.
    let plain = Builder::new_local(&local).build().await.unwrap();
    let conn = plain.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master \
             WHERE type='table' AND name='schema_migrations'",
            (),
        )
        .await
        .unwrap();
    assert!(
        rows.next().await.unwrap().is_none(),
        "schema_migrations must not exist: migrate ran before the failed sync",
    );
}

#[tokio::test]
async fn default_db_path_lives_under_mathtutor_home() {
    // Test the public helper without mutating env: just sanity-check
    // it terminates in `mt.db`. The full path depends on the env at
    // test time and is not stable across machines.
    let p: PathBuf = db::default_path().expect("default path");
    assert_eq!(p.file_name().unwrap(), "mt.db");
}
