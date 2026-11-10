# Tradeoffs

Decide these product tradeoffs before the Baserunna cutover.

## Connected OAuth sessions at cutover

The import copies OAuth clients but not tokens. Every connector must authorize again.

- **Copy refresh tokens:** Connectors keep working. Long-lived secrets move to a new store.
- **Do not copy tokens (current):** The cutover invalidates every old secret. Each connector needs one new login.

## Database backup and rollback

After the cutover, learner history exists only in Baserunna. Baserunna is alpha software, runs on one node, and has no point-in-time restore. `mt` has no export.

- **Add `mt export` and run it on a schedule:** Restores are possible. This adds a command and a storage location to operate.
- **Keep Turso read-only for a fixed period:** This costs nothing to build, but it covers only history from before the cutover.
- **Accept the risk:** No work is necessary, but a storage failure loses learner history.

## Repeated answers after an unknown outcome

Exit code 3 tells an agent not to repeat a command. Path state cannot show whether one answer was recorded, so an agent that ignores this instruction records a second review.

- **Add a card token to `mt path next` and `mt quiz answer`:** A repeated answer has no effect. Agents must pass one more field through the CLI and MCP contracts.
- **Rely on the instruction (current):** The contracts stay the same, but correct behavior depends on the agent.

## When a lesson counts as taught

`mt path next` records `lesson_taught` when it emits a lesson. If the response is lost after the commit, the lesson counts as taught although nobody saw it.

- **Record the lesson on the next call:** A lost response has no effect. The agent contract changes.
- **Record it on emission (current):** The contract stays the same, but a lost response skips a lesson.

## Agents that omit `--path`

A command without `--path` acts on the most recent path. Two concurrent sessions can move each other between paths from one command to the next.

- **Require agents to pass the path from `mt path next`:** Sessions stay on their paths. The agent contract changes.
- **Keep the default (current):** Commands stay short, but concurrent sessions can interfere.

## Schema changes across installed versions

`schema::ensure` creates absent tables and writes its own version. It cannot add a column, and an older binary overwrites a newer version.

- **Use ordered migrations and refuse a newer schema:** Upgrades become safe. An older binary stops and asks the user to upgrade.
- **Defer until the first schema change (current):** No work is necessary now, but the first change needs this design.

## Lesson status without a path

`mt graph show` without `--path` reads only the embedded curriculum, so it reports no authored lessons or quizzes.

- **Merge the overlay when a token exists:** The status is correct. Each lookup costs one network read.
- **Omit those fields without a path:** Lookups stay offline, but the fields disappear.

## Time limit for an unreachable server

The default retry policy can run for minutes against an unreachable server. Baserunna can also need time to start from zero.

- **Set a short overall deadline:** Failures are fast, but a cold start can fail a command.
- **Keep the default policy (current):** Cold starts succeed, but an outage blocks a command for minutes.

## Local-only use

`mt` no longer works offline, and `mt import-turso` accepts only an HTTP pipeline URL. A local `~/.mathtutor/mt.db` has no documented migration.

- **Add a local-file import source:** Local users can move their data. This adds code to a one-time tool.
- **Drop local-only users (current):** The code stays smaller, but local-only users lose their upgrade path.

## Retirement of the import command

The import and its tests are about 1,100 lines, including a Turso wire client and its mock.

- **Delete the command after the cutover:** The code shrinks, but self-hosted Turso users cannot migrate with a later release.
- **Keep the command (current):** Self-hosted users can migrate, and the code stays.
