//! Integration tests for `mt migrate-from-ayml`.
//!
//! Each test builds a synthetic legacy AYML tree under a `tempdir`,
//! runs `migrate::migrate`, and asserts the libSQL database matches.
//! The fixture layout mirrors `~/.mathtutor/paths/<id>/`: `path.ayml`,
//! `log.ayml`, and `overlay.ayml` per path directory.

use std::fs;
use std::path::Path;

use libsql::{Connection, params};
use mathtutor::cards;
use mathtutor::db::{self, DbConfig};
use mathtutor::migrate;
use tempfile::TempDir;

async fn fresh_db(dir: &TempDir) -> Connection {
    let cfg = DbConfig::local(dir.path().join("mt.db"));
    let db = db::open(&cfg).await.expect("open");
    db::connect(&db).await.expect("connect")
}

/// Write a file at `root/paths/<id>/<name>` with the given body, creating
/// directories as needed.
fn write_path_file(root: &Path, id: &str, name: &str, body: &str) {
    let dir = root.join("paths").join(id);
    fs::create_dir_all(&dir).expect("mkdir");
    fs::write(dir.join(name), body).expect("write");
}

async fn count(conn: &Connection, sql: &str) -> i64 {
    let mut rows = conn.query(sql, params![]).await.expect("query");
    let row = rows.next().await.unwrap().expect("row");
    row.get(0).unwrap()
}

// ── Empty / missing inputs ────────────────────────────────────────

#[tokio::test]
async fn empty_root_is_noop() {
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;
    let fixture = TempDir::new().unwrap();

    let report = migrate::migrate(&conn, fixture.path())
        .await
        .expect("migrate");
    assert_eq!(report.paths_imported, 0);
    assert_eq!(report.events_imported, 0);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM paths").await, 0);
}

#[tokio::test]
async fn missing_paths_subdir_returns_zero_report() {
    // A directory that exists but lacks `paths/` (e.g. a fresh
    // `$MATHTUTOR_HOME` from before any `mt new` ever ran) must return
    // an empty report — not error.
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;
    let fixture = TempDir::new().unwrap();
    fs::create_dir(fixture.path().join("not-paths")).unwrap();

    let report = migrate::migrate(&conn, fixture.path())
        .await
        .expect("migrate");
    assert_eq!(report.paths_imported, 0);
}

#[tokio::test]
async fn path_dir_without_path_ayml_is_skipped() {
    // A path directory missing its `path.ayml` is malformed (partial
    // `mt new`, hand-edited, etc.); migration must skip it without
    // crashing or importing orphaned overlay/log files.
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;
    let fixture = TempDir::new().unwrap();
    write_path_file(
        fixture.path(),
        "p_orphan",
        "overlay.ayml",
        "schema_version: 1\natoms: {}\n",
    );

    let report = migrate::migrate(&conn, fixture.path())
        .await
        .expect("migrate");
    assert_eq!(report.paths_imported, 0);
    assert_eq!(report.overlay_lessons_imported, 0);
}

// ── Paths + targets ───────────────────────────────────────────────

#[tokio::test]
async fn imports_path_with_targets_in_order() {
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;
    let fixture = TempDir::new().unwrap();
    write_path_file(
        fixture.path(),
        "p_test",
        "path.ayml",
        "schema_version: 1\n\
         id: p_test\n\
         goal: learn topology\n\
         created_at: 2026-05-09T17:42:00Z\n\
         target_atoms:\n\
         - tx.1.1\n\
         - tx.1.2\n\
         - tx.2.1\n",
    );

    let report = migrate::migrate(&conn, fixture.path())
        .await
        .expect("migrate");
    assert_eq!(report.paths_imported, 1);
    assert_eq!(report.paths_skipped, 0);

    let mut rows = conn
        .query(
            "SELECT goal, created_at FROM paths WHERE id = ?",
            params!["p_test"],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().expect("path row");
    assert_eq!(row.get::<String>(0).unwrap(), "learn topology");
    assert_eq!(row.get::<String>(1).unwrap(), "2026-05-09T17:42:00.000Z");

    // Targets must come back in the original insertion order, encoded
    // by the `position` column — that order is what the scheduler walks.
    let mut rows = conn
        .query(
            "SELECT atom_id FROM path_targets WHERE path_id = ? ORDER BY position ASC",
            params!["p_test"],
        )
        .await
        .unwrap();
    let mut atoms = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        atoms.push(r.get::<String>(0).unwrap());
    }
    assert_eq!(atoms, vec!["tx.1.1", "tx.1.2", "tx.2.1"]);
}

// ── Event log ─────────────────────────────────────────────────────

#[tokio::test]
async fn imports_event_log_with_payload_and_rating_columns() {
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;
    let fixture = TempDir::new().unwrap();

    write_path_file(
        fixture.path(),
        "p_test",
        "path.ayml",
        "schema_version: 1\n\
         id: p_test\n\
         goal: g\n\
         created_at: 2026-05-09T00:00:00Z\n\
         target_atoms:\n\
         - tx.1.1\n",
    );
    write_path_file(
        fixture.path(),
        "p_test",
        "log.ayml",
        "- ts: 2026-05-09T00:00:00Z\n  \
           type: path_created\n  \
           path: p_test\n\
         - ts: 2026-05-09T00:01:00Z\n  \
           type: lesson_authored\n  \
           path: p_test\n  \
           atom: tx.1.1\n\
         - ts: 2026-05-09T00:02:00Z\n  \
           type: quiz_answered\n  \
           path: p_test\n  \
           atom: tx.1.1\n  \
           quiz: tx.1.1.q1\n  \
           payload:\n    \
             rating: good\n    \
             user_answer: my answer\n",
    );

    let report = migrate::migrate(&conn, fixture.path())
        .await
        .expect("migrate");
    assert_eq!(report.events_imported, 3);

    let mut rows = conn
        .query(
            "SELECT kind, atom_id, quiz_id, rating, payload FROM events \
             WHERE path_id = ? ORDER BY id ASC",
            params!["p_test"],
        )
        .await
        .unwrap();
    let mut got = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        got.push((
            r.get::<String>(0).unwrap(),
            r.get::<Option<String>>(1).unwrap(),
            r.get::<Option<String>>(2).unwrap(),
            r.get::<Option<i64>>(3).unwrap(),
            r.get::<Option<String>>(4).unwrap(),
        ));
    }
    assert_eq!(got.len(), 3);
    assert_eq!(got[0].0, "path_created");
    assert_eq!(got[1].0, "lesson_authored");
    assert_eq!(got[1].1.as_deref(), Some("tx.1.1"));
    assert_eq!(got[2].0, "quiz_answered");
    assert_eq!(got[2].2.as_deref(), Some("tx.1.1.q1"));
    // Rating is stored in its own column, not in the JSON blob.
    assert_eq!(got[2].3, Some(3));
    assert_eq!(
        got[2].4.as_deref(),
        Some(r#"{"user_answer":"my answer"}"#),
        "payload JSON must omit rating (it lives in its own column)"
    );
}

#[tokio::test]
async fn imports_rebuild_cards_cache_from_quiz_answered_events() {
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;
    let fixture = TempDir::new().unwrap();

    write_path_file(
        fixture.path(),
        "p_test",
        "path.ayml",
        "schema_version: 1\n\
         id: p_test\n\
         goal: g\n\
         created_at: 2026-05-09T00:00:00Z\n\
         target_atoms:\n\
         - tx.1.1\n",
    );
    write_path_file(
        fixture.path(),
        "p_test",
        "log.ayml",
        "- ts: 2026-05-09T00:00:00Z\n  \
           type: quiz_answered\n  \
           path: p_test\n  \
           atom: tx.1.1\n  \
           quiz: tx.1.1.q1\n  \
           payload:\n    \
             rating: good\n",
    );

    migrate::migrate(&conn, fixture.path())
        .await
        .expect("migrate");

    // The cards write-through cache must reflect the answered quiz so
    // the scheduler can find it via the indexed `due_at` query without
    // replaying the log again.
    let card = cards::read_card(&conn, "p_test", "tx.1.1.q1")
        .await
        .expect("read_card")
        .expect("card row exists after migration");
    assert_eq!(card.reps, 1);
    assert_eq!(card.lapses, 0);
    assert!(
        card.state.stability > 0.0,
        "FSRS step must populate stability"
    );
}

// ── Overlay ───────────────────────────────────────────────────────

#[tokio::test]
async fn imports_overlay_lessons_quizzes_and_tombstones() {
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;
    let fixture = TempDir::new().unwrap();

    write_path_file(
        fixture.path(),
        "p_test",
        "path.ayml",
        "schema_version: 1\n\
         id: p_test\n\
         goal: g\n\
         created_at: 2026-05-09T00:00:00Z\n\
         target_atoms: []\n",
    );
    write_path_file(
        fixture.path(),
        "p_test",
        "overlay.ayml",
        "schema_version: 1\n\
         atoms:\n  \
           tx.1.1:\n    \
             lesson: a lesson body\n    \
             quizzes:\n    \
             - id: tx.1.1.q9\n      \
               difficulty: hard\n      \
               question: q?\n      \
               answer: a\n      \
               rubric: be precise\n    \
             removed:\n    \
             - tx.1.1.q1\n",
    );

    let report = migrate::migrate(&conn, fixture.path())
        .await
        .expect("migrate");
    assert_eq!(report.overlay_lessons_imported, 1);
    assert_eq!(report.overlay_quizzes_imported, 1);
    assert_eq!(report.overlay_removed_imported, 1);

    let mut rows = conn
        .query(
            "SELECT body FROM overlay_lessons WHERE atom_id = ?",
            params!["tx.1.1"],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().expect("lesson row");
    assert_eq!(row.get::<String>(0).unwrap(), "a lesson body");

    let mut rows = conn
        .query(
            "SELECT difficulty, kind, question, answer, rubric \
             FROM overlay_quizzes WHERE quiz_id = ?",
            params!["tx.1.1.q9"],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().expect("quiz row");
    assert_eq!(row.get::<String>(0).unwrap(), "hard");
    assert_eq!(row.get::<Option<String>>(1).unwrap(), None);
    assert_eq!(row.get::<String>(2).unwrap(), "q?");
    assert_eq!(row.get::<String>(3).unwrap(), "a");
    assert_eq!(
        row.get::<Option<String>>(4).unwrap().as_deref(),
        Some("be precise")
    );

    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM overlay_removed_quizzes WHERE quiz_id = 'tx.1.1.q1'",
        )
        .await,
        1,
    );
}

// ── Idempotency ───────────────────────────────────────────────────

#[tokio::test]
async fn second_run_skips_paths_and_does_not_duplicate_events() {
    // The core idempotency invariant for PR 4: re-running migration
    // must not duplicate path rows or event rows. The event log has no
    // natural unique key, so it gets imported only when the path itself
    // is freshly inserted — that's the test that pins the contract.
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;
    let fixture = TempDir::new().unwrap();

    write_path_file(
        fixture.path(),
        "p_test",
        "path.ayml",
        "schema_version: 1\n\
         id: p_test\n\
         goal: g\n\
         created_at: 2026-05-09T00:00:00Z\n\
         target_atoms:\n\
         - tx.1.1\n",
    );
    write_path_file(
        fixture.path(),
        "p_test",
        "log.ayml",
        "- ts: 2026-05-09T00:00:00Z\n  \
           type: path_created\n  \
           path: p_test\n\
         - ts: 2026-05-09T00:01:00Z\n  \
           type: lesson_taught\n  \
           path: p_test\n  \
           atom: tx.1.1\n",
    );
    write_path_file(
        fixture.path(),
        "p_test",
        "overlay.ayml",
        "schema_version: 1\n\
         atoms:\n  \
           tx.1.1:\n    \
             lesson: body\n",
    );

    let first = migrate::migrate(&conn, fixture.path())
        .await
        .expect("first run");
    let second = migrate::migrate(&conn, fixture.path())
        .await
        .expect("second run");

    assert_eq!(first.paths_imported, 1);
    assert_eq!(first.events_imported, 2);
    assert_eq!(first.overlay_lessons_imported, 1);

    assert_eq!(second.paths_imported, 0);
    assert_eq!(second.paths_skipped, 1);
    assert_eq!(second.events_imported, 0);
    assert_eq!(second.overlay_lessons_imported, 0);

    assert_eq!(count(&conn, "SELECT COUNT(*) FROM paths").await, 1);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM events").await, 2);
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM overlay_lessons").await,
        1
    );
}

// ── Overlay scope shift: per-path on disk → global in SQL ─────────

#[tokio::test]
async fn overlay_first_path_wins_when_atoms_collide() {
    // Legacy overlays were per-path; the SQL schema is one global table
    // keyed by atom id. When two legacy paths overlay the same atom,
    // `INSERT OR IGNORE` keeps the row from whichever path migrated
    // first. Directory listing is sorted alphabetically — `p_a` wins
    // over `p_b`.
    let tmp = TempDir::new().unwrap();
    let conn = fresh_db(&tmp).await;
    let fixture = TempDir::new().unwrap();

    for (id, lesson_body) in [("p_a", "from a"), ("p_b", "from b")] {
        write_path_file(
            fixture.path(),
            id,
            "path.ayml",
            &format!(
                "schema_version: 1\n\
                 id: {id}\n\
                 goal: g\n\
                 created_at: 2026-05-09T00:00:00Z\n\
                 target_atoms: []\n"
            ),
        );
        write_path_file(
            fixture.path(),
            id,
            "overlay.ayml",
            &format!(
                "schema_version: 1\n\
                 atoms:\n  \
                   tx.1.1:\n    \
                     lesson: {lesson_body}\n",
            ),
        );
    }

    migrate::migrate(&conn, fixture.path())
        .await
        .expect("migrate");

    let mut rows = conn
        .query(
            "SELECT body FROM overlay_lessons WHERE atom_id = ?",
            params!["tx.1.1"],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().expect("lesson row");
    assert_eq!(
        row.get::<String>(0).unwrap(),
        "from a",
        "alphabetically-first path wins under INSERT OR IGNORE",
    );
    // And only one row total — overlays are now global.
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM overlay_lessons").await,
        1,
    );
}
