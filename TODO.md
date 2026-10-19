# TODO

Follow-up tasks for the Baserunna migration.

## Before merge

- [x] **Configure Baserunna in CI.** Tests run an in-process node fetched with `BASERUNNA_DEPLOY_KEY`.
- [x] **Import existing Turso data.** Implemented in `mt import-turso`.
- [ ] **Migrate the Fly.io app.** Create the database and set `BASERUNNA_TOKEN` (and `BASERUNNA_DB` if different from `mathtutor`). Run `mt import-turso` with `TURSO_URL` and `TURSO_AUTH_TOKEN` against a scratch database first to verify zero mismatches. Deploy the app, then unset `TURSO_URL` and `TURSO_AUTH_TOKEN`.
- [ ] **Point the Baserunna dependency at `main`** after `mathtutor-load` is merged.

## Later

- [ ] **Support concurrent authoring in a shared database.** Every authoring command increments the global `overlay_version` key, which invalidates the overlay cache for all other sessions and causes transaction conflicts during concurrent edits. Incremental cache updates via the Baserunna change feed and per-atom version keys will eliminate full downloads and reduce write conflicts.
- [ ] **Run the load test against a staging database** before each deployment to verify transaction correctness and latency.
