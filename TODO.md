# TODO

Follow-up tasks for the Baserunna migration.

## Before merge

- [x] **Configure Baserunna in CI.** Tests run an in-process node built from the same Baserunna revision as the client.
- [x] **Import existing Turso data.** Implemented in `mt import-turso`.
- [ ] **Migrate the Fly.io app.** Create the database and set `BASERUNNA_TOKEN` (and `BASERUNNA_DB` if different from `mathtutor`). The deploy workflow runs only by manual dispatch until this task is complete. Run `mt import-turso` with `TURSO_URL` and `TURSO_AUTH_TOKEN` against a scratch database first to verify zero mismatches. This is the first run against real libSQL data. Revoke the libSQL credential of the old app so that it cannot write, because Fly can restart a stopped machine on the next request. Create a new token for the import. Run the import against the production database and confirm zero mismatches. Dispatch the deploy workflow, then unset `TURSO_URL` and `TURSO_AUTH_TOKEN`. Restore the `push` trigger in `.github/workflows/deploy-mcp.yml`. If an import cannot reach zero mismatches, delete the target database with an unscoped Baserunna key, correct the cause, and import again. Keep the Turso database until the new deployment is verified.
- [x] **Point the Baserunna dependencies at `main`** after the `mathtutor-client` branch of Baserunna is merged.

## Later

- [ ] **Support concurrent authoring in a shared database.** Every authoring command replaces the global `overlay_version` token, which invalidates the overlay cache for all other sessions and causes transaction conflicts during concurrent edits. Incremental cache updates via the Baserunna change feed and per-atom version keys will eliminate full downloads and reduce write conflicts.
- [ ] **Read a multi-page snapshot at one version.** `Db::snapshot` reads each page in a separate request, so a snapshot of more than 1,000 rows of one table can mix versions. Read the pages in one read-only transaction, or pin the version of the first page.
- [ ] **Run the load test against a staging database** before each deployment to verify transaction correctness and latency.
