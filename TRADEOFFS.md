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

Exit code 3 tells an agent to inspect state before a retry. The agent can still record a second review.

- **Add a card token to `mt path next` and `mt quiz answer`:** A repeated answer has no effect. Agents must pass one more field through the CLI and MCP contracts.
- **Rely on inspection (current):** The contracts stay the same, but correct behavior depends on the agent.

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
