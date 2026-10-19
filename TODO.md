# TODO

Follow-ups to moving user state from Turso to Baserunna, in priority order.

## Before merging

- [x] **Give CI Baserunna.** The tests run a node in-process from the
      private repository, a git dev-dependency that CI fetches with the
      read-only `BASERUNNA_DEPLOY_KEY` deploy key.
- [x] **Import existing Turso data.** `mt import-turso`.
- [ ] **Switch the fly.io app over.** Create the database and set
      `BASERUNNA_TOKEN` (and `BASERUNNA_DB` if not `mathtutor`). Run
      `mt import-turso` with `TURSO_URL` and `TURSO_AUTH_TOKEN` set, first
      into a scratch `BASERUNNA_DB` to see that it reports no mismatches.
      Deploy, then unset `TURSO_URL` and `TURSO_AUTH_TOKEN`.
- [ ] **Point the Baserunna dependency at `main`** once `mathtutor-load`
      merges there; the pinned commit is on that branch.

## Later

- [ ] **Concurrent authoring in one database.** Every authoring command bumps
      one overlay version key, and every command reads it to check its cached
      overlay, so commands conflict whenever another session authors at the
      same time (37% of commands with eight authoring agents in the load
      test). Each bump also makes every other client download the whole
      overlay again. Refreshing the cache incrementally from Baserunna's
      change feed would remove the downloads. Versioning the overlay per atom,
      so a command conflicts only with edits to atoms it uses, would remove
      most conflicts.
- [ ] **Run the load test against a staging database** before each deploy,
      including its exactly-once check, to catch regressions in transaction
      handling and latency.
