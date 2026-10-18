# TODO

Follow-ups to moving user state from Turso to Baserunna, in priority order.

## Before merging

- [ ] **Give CI a Baserunna binary.** The deploy workflow runs `cargo test`,
      which starts a local Baserunna node, and the Baserunna repo is private.
      Fetch a published release binary or container image with a read-only
      token, or check out and build Baserunna with a deploy key, and cache it.
- [ ] **Import existing Turso data.** Add a one-time import that reads a
      libSQL database at the last migration on `main` and writes its paths,
      authored lessons and quizzes, and each path's events into the
      Baserunna database, keeping event timestamps. Cards and lessons taught
      are then rebuilt from the events. Check the result by comparing each
      path's cards with the Turso rows.
- [ ] **Switch the fly.io app over.** Create the database, set
      `BASERUNNA_TOKEN` (and `BASERUNNA_DB` if not `mathtutor`), run the
      import, deploy, then unset `TURSO_URL` and `TURSO_AUTH_TOKEN`.

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
