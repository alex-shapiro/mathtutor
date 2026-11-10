# TODO

Follow-up tasks for the Baserunna migration.

## Before merge

- [x] **Configure Baserunna in CI.** Tests run an in-process node built from the same Baserunna revision as the client.
- [x] **Import existing Turso data.** Implemented in `mt import-turso`.
- [ ] **Migrate the Fly.io app.** Follow these steps to cut over without losing local replica writes:
  1. Create the Baserunna database and set `BASERUNNA_TOKEN` (and `BASERUNNA_DB` if different from `mathtutor`).
  2. Run `mt import-turso` with `TURSO_URL` and `TURSO_AUTH_TOKEN` against a scratch database to verify zero mismatches on real libSQL data.
  3. Stop every old Fly machine with a graceful signal and confirm that its log has zero sync failures.
  4. On each client device, run an old `mt` command to flush the local replica, confirm that it syncs, and retire the old binary.
  5. Revoke all existing libSQL tokens so that restarted Fly machines cannot write to Turso, then create a fresh token for the import.
  6. Run `mt import-turso` against the production database and confirm zero mismatches. If mismatches remain, delete the target database with an unscoped Baserunna key, fix the cause, and repeat the import.
  7. Dispatch `.github/workflows/deploy-mcp.yml`, unset `TURSO_URL` and `TURSO_AUTH_TOKEN`, and restore the `push` trigger in the workflow. Keep the Turso database until the new deployment is verified.
- [x] **Point the Baserunna dependencies at `main`** after the `mathtutor-client` branch of Baserunna is merged.

## Open product decisions

### Connected OAuth sessions at cutover

`mt import-turso` copies OAuth clients without access or refresh tokens, so every connector must sign in again after cutover.

- **Copy refresh tokens:** Connectors stay signed in, and long-lived secrets move to the new store.
- **Omit tokens (current):** The cutover invalidates existing secrets, and each connector signs in again.

### Database backup and rollback

After cutover, learner history is stored only in Baserunna, which runs on a single node without point-in-time recovery or an `mt` export command.

- **Add `mt export` and run it on a schedule:** Restores are possible, at the cost of operating a backup store.
- **Keep Turso read-only for a fixed period:** Requires zero new code and preserves pre-cutover history.
- **Accept the risk:** Requires zero work, and a storage failure loses learner history.

### Repeated answers after an unknown outcome

Exit code `3` instructs the agent not to repeat a command whose outcome is unknown, because path state does not distinguish a single answer from a duplicate.

- **Add an idempotency token to `mt path next` and `mt quiz answer`:** Duplicate submissions become no-ops, and agents pass an extra field through the CLI and MCP contracts.
- **Rely on the instruction (current):** The contracts stay unchanged, and correctness depends on agent compliance.

### When a lesson counts as taught

`mt path next` records `lesson_taught` when it emits a lesson, so a lost response marks the lesson as taught before the user sees it.

- **Record the lesson on the next call:** A lost response has zero effect, and the agent contract changes.
- **Record on emission (current):** The contract stays unchanged, and a lost response skips the lesson.

### Agents that omit `--path`

Commands without `--path` act on the most recent path, and every write that selects a path updates its activity timestamp. Concurrent sessions that omit `--path` can still switch each other's active path between commands.

- **Require agents to pass `--path` from `mt path next`:** Concurrent sessions stay isolated, and the agent contract changes.
- **Keep the default (current):** Commands stay short, and concurrent sessions can interfere.

### Cards that FSRS 6.1 cannot reproduce

FSRS 6.1.0 clamps same-day `hard` stability growth, whereas 5.2.0 did not. Cards with same-day `hard` reviews from before 2026-06-01 replay to a different stability, which leaves the import open with a mismatch.

- **Report FSRS parameter drift without blocking if `reps`, `lapses`, and timestamps match:** The import completes, and affected cards adopt the 6.1.0 schedule.
- **Copy FSRS state directly from libSQL:** Schedules stay identical, and imported cards diverge from a pure event replay.
- **Add a flag to accept mismatches:** Gives the operator an explicit override for inspected differences.

### Correction of recorded reviews

The event log is append-only and has no command to void a mistyped or duplicate review.

- **Add a compensating event and rebuild projections:** Mistakes can be undone, and the system must define authorization rules.
- **Keep the log append-only (current):** The data model stays simple, and recorded reviews are permanent.

### Schema changes across installed versions

`schema::ensure` creates missing tables and writes its schema version, which does not add columns to existing tables or prevent an older binary from overwriting a newer version marker.

- **Run ordered migrations and reject newer schemas:** Upgrades become additive, and older binaries exit with an upgrade prompt.
- **Defer until the next schema change (current):** Avoids migration machinery until the schema changes.

### Lesson status without a path

`mt graph show` without `--path` reads only the embedded curriculum and ignores user-authored lessons and quizzes in the overlay.

- **Merge the overlay if `BASERUNNA_TOKEN` is set:** Output reflects authored content at the cost of a network read.
- **Omit `has_lesson` and `quizzes` when `--path` is absent:** Lookups stay offline without reporting stale overlay counts.

### Time limit for an unreachable server

The default client retry policy can wait several minutes for an unreachable server in order to tolerate cold starts.

- **Set a short overall deadline:** Outages fail fast, and cold starts can time out.
- **Keep the default policy (current):** Cold starts succeed, and outages block commands for minutes.

### Local-only use

`mt` requires a Baserunna server, and `mt import-turso` reads only from an HTTP pipeline URL rather than a local `~/.mathtutor/mt.db` file.

- **Add a local-file import source:** Local users can migrate their SQLite files.
- **Require an HTTP source (current):** Keeps the import tool smaller.

### Retirement of the import command

`src/import.rs` and `tests/import.rs` add roughly 1,100 lines for the Turso wire client and test harness.

- **Delete `mt import-turso` after cutover:** Shrinks the codebase once production data is migrated.
- **Keep the command (current):** Retains a migration path for other Turso databases.

### Verification of derived rows after cutover

`cards` and `taught` drive scheduling, and only `mt import-turso` verifies them against the event stream.

- **Add a command to verify and rebuild projections:** Operators can detect and repair drift.
- **Omit the command (current):** Keeps the CLI smaller until projection drift is observed.

### Acceptance test for the cutover

Reverting to Turso after Baserunna accepts new writes loses those writes.

- **Define a pre-traffic checklist:** Verify the imported path count, an OAuth login, and a test `get_next` / `answer_quiz` cycle before enabling user traffic.
- **Rely on the operator (current):** Keeps the runbook shorter.

## Later

- [ ] **Support concurrent authoring in a shared database.** Authoring commands replace the global `overlay_version` token, which invalidates the overlay cache for other sessions and causes transaction conflicts during concurrent edits. Incremental cache updates via the Baserunna change feed and per-atom version keys will avoid full downloads and reduce write conflicts.
- [ ] **Read a multi-page snapshot at a single version.** `Db::snapshot` reads each page in a separate request, so a table with more than 1,000 rows can span versions across pages. Read all pages in a single read-only transaction or pin the snapshot version from the first page.
- [ ] **Run the load test against a staging database** before each deployment to verify transaction correctness and latency.
