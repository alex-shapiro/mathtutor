# TODO

Follow-up tasks for the Baserunna migration.

## Before merge

- [x] **Configure Baserunna in CI.** Tests run an in-process node built from the same Baserunna revision as the client.
- [x] **Import existing Turso data.** Implemented in `mt import-turso`.
- [ ] **Migrate the Fly.io app.** Follow these steps to cut over without losing local replica writes:
  1. Create a temporary Baserunna key without a database limit and set it as `BASERUNNA_TOKEN` for the imports. Only such a key can create the target database, and it can also delete every database of its user, so keep it off the server and the devices. Set `BASERUNNA_URL` to the production Baserunna server, which the `[env]` section of `fly.toml` also names. Set `BASERUNNA_DB` if the database name is not `mathtutor`.
  2. Run `mt import-turso` with `TURSO_URL` and `TURSO_AUTH_TOKEN` against a scratch database to verify zero mismatches on real libSQL data.
  3. Stop every old Fly machine with a graceful signal and confirm that its log has zero sync failures.
  4. On each client device, run an old `mt` command to flush the local replica, confirm that it syncs, and retire the old binary.
  5. Set `MT_API_KEY` to a new random key of at least 32 characters, such as with `flyctl secrets set --stage MT_API_KEY="$(openssl rand -base64 32)"`. The new server refuses to start with a shorter key, and `--stage` keeps the stopped machines stopped. Give the new key to each client that uses `MT_API_KEY`.
  6. Revoke all existing libSQL tokens so that restarted Fly machines cannot write to Turso, then create a fresh token for the import.
  7. Run `mt import-turso` against the production database and confirm zero mismatches. If mismatches remain, delete the target database with an unscoped Baserunna key, fix the cause, and repeat the import.
  8. Create a key limited to the production database for the server, with its own rate limit as "Deployment" in `docs/mcp.md` shows, and a separate limited key for each device. Set the server key as the `BASERUNNA_TOKEN` secret of the Fly app, and delete the temporary key of step 1. See "Baserunna keys" in `docs/design.md`.
  9. Check that `BASERUNNA_URL` in the `[env]` section of `fly.toml` names the production Baserunna server. Dispatch `.github/workflows/deploy-mcp.yml`, unset `TURSO_URL` and `TURSO_AUTH_TOKEN`, and restore the `push` trigger in the workflow. Keep the Turso database until the new deployment is verified.
- [x] **Point the Baserunna dependencies at `main`** after the `mathtutor-client` branch of Baserunna is merged.

## Open product decisions

### Connected OAuth sessions at cutover

`mt import-turso` copies only OAuth client registrations. Every connector must sign in again after cutover.

- **Copy refresh tokens:** Connectors stay signed in. Long-lived secrets move to the new store.
- **Omit tokens:** The cutover invalidates existing secrets. Each connector signs in again.

### Database backup and rollback

After cutover, Baserunna is the only store for learner history. It runs on a single node without point-in-time recovery or an `mt` export command.

- **Add `mt export` and run it on a schedule:** Restores are possible. This option requires a backup store.
- **Keep Turso read-only for a fixed period:** Requires zero new code and preserves pre-cutover history.
- **Accept the risk:** Requires zero work. A storage failure loses learner history.

### Repeated answers after an unknown outcome

Exit code `3` tells the agent to advance instead of repeating a command with an unknown outcome. Path state cannot distinguish a single answer from a duplicate. The recovery step for `mt path next` and `get_next` is a repeat, and a repeat after a lost response logs a second `quiz_presented` event, which raises the presentation count that the envelope reports.

- **Add an idempotency token to `mt path next` and `mt quiz answer`:** Duplicate submissions become no-ops. Agents must pass an extra field through the CLI and MCP contracts.
- **Rely on the instruction:** The contracts stay unchanged. Correctness depends on agent compliance.

### Recording a taught lesson

`mt path next` records `lesson_taught` as soon as it emits a lesson. A lost response marks the lesson as taught before the user sees it.

- **Record the lesson on the next call:** A lost response has zero effect. The agent contract changes.
- **Record on emission:** The contract stays unchanged. A lost response skips the lesson.

### Agents that omit `--path`

Commands without `--path` act on the most recent path. Every write to a path updates its activity timestamp. Concurrent sessions that omit `--path` can switch each other's active path between commands.

- **Require agents to pass `--path` from `mt path next`:** Concurrent sessions stay isolated. The agent contract changes.
- **Keep the default:** Commands stay short. Concurrent sessions can interfere.

### Cards that FSRS 6.1 cannot reproduce

FSRS 6.1.0 clamps same-day `hard` stability growth. FSRS 5.2.0 did not clamp this growth. Cards with same-day `hard` reviews from before 2026-06-01 replay to a different stability. This difference leaves the import open with a mismatch.

- **Report FSRS parameter drift without blocking if `reps`, `lapses`, and timestamps match:** The import completes. Affected cards adopt the 6.1.0 schedule.
- **Copy FSRS state directly from libSQL:** Schedules stay identical. Imported cards diverge from a pure event replay.
- **Add a flag to accept mismatches:** Gives the operator an explicit override for inspected differences.

### Correction of recorded reviews

The event log is append-only and lacks a command to void a mistyped or duplicate review.

- **Add a compensating event and rebuild projections:** Mistakes can be undone. The system must define authorization rules.
- **Keep the log append-only:** The data model stays simple. Recorded reviews are permanent.

### Schema changes across installed versions

`Db::create_if_missing` creates missing tables and writes the schema version. It does not add columns to existing tables or block older binaries from overwriting a newer version marker.

The schema version covers only the shape of the tables. It does not cover the logic that computes `cards` rows: the FSRS crate version and `cards::apply_event`. Devices on different releases write cards that different logic computed, and no check detects stale rows. Record a projection version beside the schema version, and rebuild `cards` from the event streams when the version changes.

- **Run ordered migrations and reject newer schemas:** Upgrades become additive. Older binaries exit with an upgrade prompt.
- **Defer until the next schema change:** Avoids migration machinery until the schema changes.

### Lesson status without a path

`mt graph show` without `--path` reads only the embedded curriculum and ignores user-authored lessons and quizzes in the overlay.

- **Merge the overlay if `BASERUNNA_TOKEN` is set:** Output reflects authored content at the cost of a network read.
- **Omit `has_lesson` and `quizzes` if `--path` is absent:** Lookups stay offline without reporting stale overlay counts.

### Time limit for an unreachable server

The default client retry policy can wait several minutes for an unreachable server to tolerate cold starts.

- **Set a short overall deadline:** Outages fail fast. Cold starts can time out.
- **Keep the default policy:** Cold starts succeed. Outages block commands for minutes.

### Local-only use

`mt` requires a Baserunna server. `mt import-turso` reads only from an HTTP pipeline URL.

- **Add a local-file import source:** Local users can migrate their SQLite files.
- **Require an HTTP source:** Keeps the import tool smaller.

### Retirement of the import command

`src/import.rs` and `tests/import.rs` add roughly 1,100 lines for the Turso wire client and test harness.

- **Delete `mt import-turso` after cutover:** Shrinks the codebase once production data is migrated.
- **Keep the command:** Retains a migration path for other Turso databases.

### Verification of derived rows after cutover

`cards` and `taught` drive scheduling. Only `mt import-turso` verifies them against the event stream.

- **Add a command to verify and rebuild projections:** Operators can detect and repair drift.
- **Omit the command:** Keeps the CLI smaller until projection drift is observed.

### Acceptance test for the cutover

Reverting to Turso after Baserunna accepts new writes loses those writes.

- **Define a pre-traffic checklist:** Verify the imported path count, an OAuth login, and a test `get_next` / `answer_quiz` cycle before enabling user traffic.
- **Rely on the operator:** Keeps the runbook shorter.

## Later

- [ ] **Protect learner history from a leaked key.** This is a preexisting limit of Baserunna. Every key that can write learner state can also drop the tables and delete or trim the event streams, because Baserunna gives these actions to the writer role and has no per-key role. A key limited to the database cannot delete the database, but it can destroy its contents. Baserunna has no backup or point-in-time restore. Ask Baserunna for a role that writes rows and appends to streams without schema or stream deletion, and add a backup (see "Database backup and rollback").

- [ ] **Support concurrent authoring in a shared database.** Authoring commands replace the global `overlay_version` token. This replacement invalidates the overlay cache for other sessions and causes transaction conflicts during concurrent edits. Use the Baserunna change feed and per-atom version keys to update caches incrementally and reduce write conflicts.
- [ ] **Read a multi-page snapshot at a single version.** `Db::snapshot` reads each page in a separate request. A table with more than 1,000 rows can span versions across pages. Read all pages in a single read-only transaction, or pin the snapshot version from the first page.
- [ ] **Run the load test against a staging database** before each deployment to verify transaction correctness and latency.
- [ ] **Protect learner data from curriculum ID changes.** Stored targets, subpaths, overlay rows, and cards use the curriculum IDs that each binary embeds. A release that renames or removes an ID silently drops authored content for that atom, makes every command on a path with that target fail with `unknown_id`, and gives devices on different releases different graphs. Add a `renamed_from` alias that `mt` applies to stored data, or a release check that fails when a release removes an ID that a database still uses. Both options add a migration cost to curriculum edits.
- [ ] **Remove test placeholders from the shipped curriculum.** Quiz `fnd.1.1.1.q1` asks "Q?" with the answer "A", and a shell expanded `$S` in quiz `fnd.2.1.1.q1`. Tests use the production curriculum as a fixture, so these quizzes stay. Move the test fixtures to a test curriculum, and make `mt graph check` reject placeholder text and unbalanced `$`.
- [ ] **Detect a Baserunna server that differs from the pinned client.** CI tests against an in-process node at the pinned revision, but every device and the server use the live deployment. A Baserunna deploy that changes its behavior breaks every client at once, with errors that do not mention versions. Add a scheduled smoke test against a staging database on the live server, and a check of the server API version on the first request.
