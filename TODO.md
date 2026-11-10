# TODO

Follow-up tasks for the Baserunna migration.

## Before merge

- [x] **Configure Baserunna in CI.** Tests run an in-process node built from the same Baserunna revision as the client.
- [x] **Import existing Turso data.** Implemented in `mt import-turso`.
- [ ] **Migrate the Fly.io app.** Create the database and set `BASERUNNA_TOKEN` (and `BASERUNNA_DB` if different from `mathtutor`). The deploy workflow runs only by manual dispatch until this task is complete. Run `mt import-turso` with `TURSO_URL` and `TURSO_AUTH_TOKEN` against a scratch database first to verify zero mismatches. This is the first run against real libSQL data. Old `mt` processes write to a local replica first, so stop every old Fly machine with a graceful signal and confirm that its log has no sync failure. On each device, run one old `mt` command, confirm that it syncs, and stop using the old binary. Then revoke every libSQL token, because Fly can restart a stopped machine on the next request. Create a new token for the import. Run the import against the production database and confirm zero mismatches. Dispatch the deploy workflow, then unset `TURSO_URL` and `TURSO_AUTH_TOKEN`. Restore the `push` trigger in `.github/workflows/deploy-mcp.yml`. If an import cannot reach zero mismatches, delete the target database with an unscoped Baserunna key, correct the cause, and import again. Keep the Turso database until the new deployment is verified.
- [x] **Point the Baserunna dependencies at `main`** after the `mathtutor-client` branch of Baserunna is merged.

## Open product decisions


### Connected OAuth sessions at cutover

The import copies OAuth clients but not tokens. Every connector must authorize again.

- **Copy refresh tokens:** Connectors keep working. Long-lived secrets move to a new store.
- **Do not copy tokens (current):** The cutover invalidates every old secret. Each connector needs one new login.

### Database backup and rollback

After the cutover, learner history exists only in Baserunna. Baserunna is alpha software, runs on one node, and has no point-in-time restore. `mt` has no export.

- **Add `mt export` and run it on a schedule:** Restores are possible. This adds a command and a storage location to operate.
- **Keep Turso read-only for a fixed period:** This costs nothing to build, but it covers only history from before the cutover.
- **Accept the risk:** No work is necessary, but a storage failure loses learner history.

### Repeated answers after an unknown outcome

Exit code 3 tells an agent not to repeat a command. Path state cannot show whether one answer was recorded, so an agent that ignores this instruction records a second review.

- **Add a card token to `mt path next` and `mt quiz answer`:** A repeated answer has no effect. Agents must pass one more field through the CLI and MCP contracts.
- **Rely on the instruction (current):** The contracts stay the same, but correct behavior depends on the agent.

### When a lesson counts as taught

`mt path next` records `lesson_taught` when it emits a lesson. If the response is lost after the commit, the lesson counts as taught although nobody saw it.

- **Record the lesson on the next call:** A lost response has no effect. The agent contract changes.
- **Record it on emission (current):** The contract stays the same, but a lost response skips a lesson.

### Agents that omit `--path`

A command without `--path` acts on the most recent path. Every write that selects a path makes it the most recent, so one session stays on its path. Two concurrent sessions can still move each other between paths from one command to the next.

- **Require agents to pass the path from `mt path next`:** Sessions stay on their paths. The agent contract changes.
- **Keep the default (current):** Commands stay short, but concurrent sessions can interfere.

### Cards that FSRS 6.1 cannot reproduce

FSRS 6.1.0 clamps same-day `hard` growth, and 5.2.0 does not. A card with such a review from before 2026-06-01 replays to a different stability, so the import reports a difference and stays open. No real data has been checked yet.

- **Report FSRS differences without blocking, and still require matching `reps`, `lapses`, and review times:** The import can finish. Affected cards get the 6.1.0 schedule.
- **Copy FSRS state from libSQL instead of rebuilding it:** Schedules stay exact. Those rows stop being a pure projection of events.
- **Add an option that accepts differences:** This also gives a general escape from a blocked import. The operator must judge each difference.

### Correction of recorded reviews

No command can void a review. A duplicate answer or a wrong grade changes a card's schedule permanently.

- **Add a compensating event and a projection rebuild:** Errors can be repaired. Someone must decide who can void a review.
- **Keep the log append-only with no correction (current):** The code stays simple, but errors are permanent.

### Schema changes across installed versions

`schema::ensure` creates absent tables and writes its own version. It cannot add a column, and an older binary overwrites a newer version.

- **Use ordered migrations and refuse a newer schema:** Upgrades become safe. An older binary stops and asks the user to upgrade.
- **Defer until the first schema change (current):** No work is necessary now, but the first change needs this design.

### Lesson status without a path

`mt graph show` without `--path` reads only the embedded curriculum, so it reports no authored lessons or quizzes.

- **Merge the overlay when a token exists:** The status is correct. Each lookup costs one network read.
- **Omit those fields without a path:** Lookups stay offline, but the fields disappear.

### Time limit for an unreachable server

The default retry policy can run for minutes against an unreachable server. Baserunna can also need time to start from zero.

- **Set a short overall deadline:** Failures are fast, but a cold start can fail a command.
- **Keep the default policy (current):** Cold starts succeed, but an outage blocks a command for minutes.

### Local-only use

`mt` no longer works offline, and `mt import-turso` accepts only an HTTP pipeline URL. A local `~/.mathtutor/mt.db` has no documented migration.

- **Add a local-file import source:** Local users can move their data. This adds code to a one-time tool.
- **Drop local-only users (current):** The code stays smaller, but local-only users lose their upgrade path.

### Retirement of the import command

The import and its tests are about 1,100 lines, including a Turso wire client and its mock.

- **Delete the command after the cutover:** The code shrinks, but self-hosted Turso users cannot migrate with a later release.
- **Keep the command (current):** Self-hosted users can migrate, and the code stays.

### Verification of derived rows after the cutover

`cards` and `taught` decide what a learner sees next, but only the import compares them with events.

- **Add a command that replays each path and can rebuild it:** Operators can detect and repair drift. This adds an operator command.
- **Do not add a check (current):** The code stays smaller, but a projection bug changes schedules with no signal.

### Acceptance test for the cutover

The runbook says to keep Turso until the deployment is verified, but it does not define that check. Going back after the first Baserunna write loses that write.

- **Write a checklist with an abort point:** Check the imported path count, one OAuth login, and one `get_next` and `answer_quiz` on a test path. This adds a manual step.
- **Rely on the operator (current):** The cutover is faster, but the last safe abort is unclear.

## Later

- [ ] **Support concurrent authoring in a shared database.** Every authoring command replaces the global `overlay_version` token, which invalidates the overlay cache for all other sessions and causes transaction conflicts during concurrent edits. Incremental cache updates via the Baserunna change feed and per-atom version keys will eliminate full downloads and reduce write conflicts.
- [ ] **Read a multi-page snapshot at one version.** `Db::snapshot` reads each page in a separate request, so a snapshot of more than 1,000 rows of one table can mix versions. Read the pages in one read-only transaction, or pin the version of the first page.
- [ ] **Run the load test against a staging database** before each deployment to verify transaction correctness and latency.
