# TODO

Follow-up tasks for the Baserunna migration.

## Before merge

- [x] **Configure Baserunna in CI.** Tests run an in-process node built from the same Baserunna revision as the client.
- [x] **Import existing Turso data.** Implemented in `mt import-turso`.
- [ ] **Migrate the Fly.io app.** Follow these steps to cut over without losing local replica writes:
  1. Create a temporary unrestricted Baserunna key and set it as `BASERUNNA_TOKEN` for the imports. Only an unrestricted key can create the target database. Keep it off the server and the devices because it can also delete every database of its user. Set `BASERUNNA_URL` to the production Baserunna server configured in `fly.toml`. Set `BASERUNNA_DB` if the database name is not `mathtutor`.
  2. Run `mt import-turso` with `TURSO_URL` and `TURSO_AUTH_TOKEN` against a scratch database to verify zero mismatches on real libSQL data.
  3. Stop every old Fly machine with a graceful signal. Confirm that its log has zero sync failures.
  4. On each client device, run an old `mt` command to flush the local replica. Confirm that it syncs, and retire the old binary.
  5. Set `MT_API_KEY` to a new random key of at least 32 characters, such as with `flyctl secrets set --stage MT_API_KEY="$(openssl rand -base64 32)"`. The new server fails at startup with a shorter key. `--stage` keeps the stopped machines stopped. Give the new key to each client that uses `MT_API_KEY`.
  6. Revoke all existing libSQL tokens so that restarted Fly machines cannot write to Turso. Create a fresh token for the import.
  7. Run `mt import-turso` against the production database and confirm zero mismatches. If mismatches remain, delete the target database with an unrestricted Baserunna key, fix the cause, and repeat the import.
  8. Create a database-limited key for the server with its own rate limit, as shown under "Deployment" in `docs/mcp.md`. Create a separate limited key for each device. Set the server key as the `BASERUNNA_TOKEN` secret of the Fly app. Delete the temporary key from step 1. See "Baserunna keys" in `docs/design.md`.
  9. Check that `BASERUNNA_URL` in the `[env]` section of `fly.toml` specifies the production Baserunna server. Dispatch `.github/workflows/deploy-mcp.yml`, unset `TURSO_URL` and `TURSO_AUTH_TOKEN`, and restore the `push` trigger in the workflow. Keep the Turso database until the new deployment is verified.
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

Exit code `3` tells the agent to advance instead of repeating a command with an unknown outcome. Path state cannot distinguish a single answer from a duplicate. The recovery step for `mt path next` and `get_next` is a repeat. A repeat after a lost response logs a second `quiz_presented` event and raises the presentation count in the envelope.

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

The schema version covers only the table definitions. It does not cover the FSRS crate version or `cards::apply_event`. Devices on different releases write cards computed by different logic without any check for stale rows. Record a projection version beside the schema version. Rebuild `cards` from the event streams if the version changes.

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

### A lost refresh response signs the connector out

A refresh request to `/oauth/token` deletes the presented refresh token in the transaction that issues the new pair. If the response does not reach the connector, the connector holds only the deleted token. Its next refresh fails, and the user must sign in again.

- **Keep the old token valid for a short grace window:** A lost response has zero effect. A stolen refresh token stays usable for longer.
- **Return the same new pair if the old token comes back within the window:** A lost response has zero effect, and the server does not issue an extra secret. A stolen refresh token has the same longer exposure.
- **Keep strict rotation:** A stolen token works only once. A lost response signs the connector out.

### Who creates the database

The first `mt path new` creates the database. Only an unrestricted key can create a database. That key can also delete every database of its user. Thus the first session of an agent must hold a key that can delete all learner history.

- **Add an operator setup command, such as `mt db create`:** The operator runs it once with a temporary unrestricted key. After that, every learner command uses a database-limited key. A new user has an extra setup step.
- **Keep creation in `mt path new`:** Setup avoids an extra step. The agent holds an unrestricted key on first use.
- **Create the database in the Baserunna console without creating databases in `mt`:** The user clicks "Create database" in the console. Every command uses a database-limited key, and the first command creates the tables. A typo in `BASERUNNA_DB` specifies a missing database, so the command fails without starting a second history. `mt import-turso` does not need a temporary unrestricted key. Audit 4 confirmed that a key limited to a console-created database can create the tables and the first path.

The current behavior also blocks the first agent session. The playbook starts with `mt path state`, which fails on a new setup because the database does not exist yet. The error tells the agent to check `BASERUNNA_DB` and `BASERUNNA_URL`, so the agent stops before `mt path new`. An MCP-only user cannot pass this step. `mt mcp` starts, but `/ready` fails and every tool call fails with a `BASERUNNA_DB` error. Only the console step or a CLI-created path clears this error. A setup command removes both problems. A creation hint in the error removes them too, but an agent that follows the hint after a `BASERUNNA_DB` typo starts a second, empty learner history.

### Devices as full database clients

The CLI on each device and the MCP server are full database clients. Each client holds a Baserunna key, writes the derived `cards` and `taught` rows with its own FSRS version, checks the schema version, and downloads the full overlay after each edit. This design causes four problems listed under "Later": a leaked device key can drop tables, every device must run the same release, devices on different releases compute cards differently, and each device downloads the full overlay.

- **Make the CLI a client of the server API:** Only the server holds a Baserunna key, writes projections, and checks the schema. Each device holds a revocable server token that cannot drop tables. The CLI requires a running `mt mcp` server, a Fly cold start delays the first command, and a single-device user must run or host a server.
- **Keep devices as full clients:** A server is unnecessary. The four problems remain and require separate fixes.

### Sign-in delay after incorrect passwords

`/oauth/authorize` delays the next attempt from an address after 5 incorrect passwords. The wait doubles from 1 second up to a 15-minute limit, so an attacker at the limit can try about 4 passwords per hour per address. The limit also applies to the administrator on the same address and to every user behind the same NAT or `/64` IPv6 network. The server uses a 15-minute limit now.

- **Shorter limit, such as 1 minute:** A user who mistypes many times waits less. An attacker can try about 60 passwords per hour per address.
- **Longer limit, such as 1 hour:** An attacker can try 1 password per hour per address. A user who mistypes many times can wait up to 1 hour.
- **Add a global limit on failures from all addresses:** An attacker with many addresses gets few attempts. An attacker can lock out the administrator from every address.

### Default Baserunna server

`BASERUNNA_URL` defaults to `https://baserunna.fly.dev`, the maintainer's deployment. A user who omits `BASERUNNA_URL` sends another server's API key to this deployment. The deployment rejects the key with HTTP 401, and `mt` reports the URL and the two settings to check.

- **Remove the default and fail with exit code `2` for `BASERUNNA_URL`:** Keys never go to an unintended server. Each user sets an extra variable.
- **Keep the default as a hosted service and state its limits in the README:** Setup stays short. Users depend on a single unbacked node operated by the maintainer.
- **Keep the default only for the maintainer and document it:** Setup stays short for the maintainer. Other users can send their key to the maintainer's server by mistake.

## Later

- [ ] **Protect learner history from a leaked key.** Baserunna gives every writer key permission to drop tables and delete or trim event streams. A database-limited key cannot delete the database, but it can destroy its contents. Baserunna also lacks backups and point-in-time restore. Request a Baserunna role that writes rows and appends to streams without schema or stream deletion. Add a backup as described under "Database backup and rollback".

- [ ] **Support concurrent authoring in a shared database.** Authoring commands replace the global `overlay_version` token. This replacement invalidates the overlay cache for other sessions and causes transaction conflicts during concurrent edits. Use the Baserunna change feed and per-atom version keys to update caches incrementally and reduce write conflicts.
- [ ] **Read a multi-page snapshot at a single version.** `Db::snapshot` reads each page in a separate request. A table with more than 1,000 rows can span versions across pages. Read all pages in a single read-only transaction, or pin the snapshot version from the first page.
- [ ] **Run the load test against a staging database** before each deployment to verify transaction correctness and latency.
- [ ] **Protect learner data from curriculum ID changes.** Stored targets, subpaths, overlay rows, and cards use the curriculum IDs embedded in each binary. A release that renames or removes an ID drops authored content for that atom, makes commands on paths with that target fail with `unknown_id`, and gives devices on different releases different graphs. Add a `renamed_from` alias that `mt` applies to stored data, or add a release check that fails if a release removes an ID in use. Both options add a migration cost to curriculum edits.
- [ ] **Remove test placeholders from the shipped curriculum.** Quiz `fnd.1.1.1.q1` asks "Q?" with the answer "A". A shell expanded `$S` in quiz `fnd.2.1.1.q1`. Tests use the production curriculum as a fixture, so these quizzes remain. Move the test fixtures to a test curriculum. Make `mt graph check` reject placeholder text and unbalanced `$`.
- [ ] **Detect a Baserunna server that differs from the pinned client.** CI tests against an in-process node at the pinned revision, whereas devices and the server use the live deployment. A Baserunna deploy that changes behavior breaks every client at once without mentioning versions. Add a scheduled smoke test against a staging database on the live server. Check the server API version on the first request.
- [ ] **Give imported user quizzes IDs that a curriculum release cannot reuse.** `mt import-turso` keeps the `<atom>.qN` IDs of user quizzes. If a later release adds a curriculum quiz with the same ID, the overlay row replaces that curriculum quiz. Rename imported user quizzes to `<atom>.uN` in quizzes, tombstones, cards, and events. Update import verification to accept the new IDs.
- [ ] **Use a single clock for event timestamps.** Each device and the server write events with their own clock, and FSRS computes due times from those events. A fast device clock delays reviews on every device, and a slow clock brings them forward. Take the timestamp from the Baserunna commit if the server can return it. At minimum, reject or clamp an answer timestamp earlier than the card's last review.
- [ ] **Alert the operator if the server is unhealthy.** A failed `/ready` check stops a deploy, but nothing alerts the operator after deployment. Both Fly apps scale to zero, so a revoked key, a Baserunna outage, or a schema mismatch surfaces only if a learner gets an error. Add a scheduled external check of `/ready`, such as a GitHub Actions cron job or an uptime service.
- [ ] **Let the operator sign out OAuth connectors.** Refresh tokens rotate and gain 30 days on each refresh, so an active connector stays signed in indefinitely. The server lacks a revocation endpoint, and changing `MT_ADMIN_PASSWORD` does not invalidate stored tokens. A lost device or leaked refresh token retains write access to learner history. Add `mt mcp revoke` for all tokens or for a specific client, or reject grants issued under an older password. The second option signs out every connector if the password changes. Delete the client registrations of revoked connectors as well, because registrations with a token grant do not expire.
- [ ] **Import paths that exceed a single transaction.** `mt import-turso` writes each path and its events in a single transaction. Baserunna limits a transaction to 16 MB of writes and 10,000 operations, so a larger path fails and leaves the import open. Check the size of each path before claiming the target database, and list any oversized paths. Ensure that splitting a path across transactions does not append duplicate events on resume.
- [ ] **Version the overlay cache file.** The cache file stores quizzes in their curriculum-file format without a version number. A release that changes that format with serde defaults can decode an old cache file without error and serve stale content until the next overlay edit. Include a format version or the crate version in the cache file name.
