# Math Tutor Design

Math Tutor manages math learning through a directed acyclic graph of lessons and quizzes with spaced repetition. The curriculum graph is compiled into the `mt` binary. User state is stored in a [Baserunna](https://baserunna.fly.dev) database. Curriculum files and CLI output use [AYML](https://crates.io/crates/ayml).

## Responsibilities

`mt` is invoked by an LLM agent:

- `mt` manages scheduling, persistence, graph validation, and the user overlay.
- The agent authors lessons and quizzes, presents material to the user, and grades responses.

`mt path next` selects the next action for the agent to present. If a lesson or quiz is required for the first time, the agent authors it and saves it with `mt lesson upsert` or `mt quiz create`. All authored content is written to the user overlay so that subsequent presentations are deterministic.

Each atom represents a single concept. A lesson is 1–2 paragraphs and introduces at most a single theorem, rule, or definition. Quizzes depend only on the current lesson and completed prerequisites.

## Commands

```bash
# Path lifecycle
mt path list
mt path new <GOAL> --targets <ID>[,<ID>...] [--strategy {bottom_up,top_down}]
mt path state [--path P]
mt path next  [--path P] [--new | --due]
mt path strategy {bottom_up,top_down} [--path P]
mt path subpath set --atoms <ID>[,<ID>...] [--path P]
mt path subpath clear [--path P]
mt path syllabus [--path P] [-n N]
mt path tree  [--path P] [--depth N]

# Curriculum lookup, read-only
mt graph list  [<ID>] [--path P] [--recursive]
mt graph show  <ID>   [--path P]
mt graph check [-p DIR]
mt graph dump

# Authoring, writes to the user overlay
mt lesson upsert <ATOM>    --body TEXT
mt quiz   create <ATOM>    --difficulty D \
                           --question TEXT --answer TEXT [--rubric TEXT] \
                           [--type {free_text,multiple_choice}]
mt quiz   update <QUIZ_ID> [--question TEXT] [--answer TEXT] [--rubric TEXT] \
                           [--difficulty D] [--type T]
mt quiz   delete <QUIZ_ID>

# User outcomes
mt quiz answer <QUIZ_ID> --rating {again,hard,good,easy} [--user-answer TEXT]

# Playbook and migration
mt instruct
mt import-turso [--url URL]
```

`--new` skips due reviews. `--due` returns only due reviews. See "Spaced repetition".

Each `TEXT` value is a single shell word. Put the text in single quotes, and write each single quote in the text as `'\''`. The shell then keeps `$`, `\`, `"`, and line breaks unchanged, so LaTeX such as `$x^2$` reaches `mt` intact. The `next_step` field and the agent playbook use this form.

If `--path P` is passed to `mt graph show` or `mt graph list`, the output includes `lesson_taught`, `complete`, and the `easy`, `medium`, and `hard` quiz slots for each atom on that path. Each slot is `missing`, `authored`, or `correct`. `mt graph list --recursive` also lists every descendant of the node.

### Target ID resolution

Each `--targets` entry is either an atom ID, such as `tx.1.1`, or a cluster ID, such as `tx.1`. A cluster ID expands to all of its atomic descendants.

`mt path new` validates that each entry expands to at least a single atom and stores the raw IDs in the `paths` table. Target IDs and subpath IDs expand to a deduplicated, topologically sorted list of atoms each time the path loads. If an atom is later split into a cluster in the curriculum, the stored ID expands to the new descendant atoms. If the curriculum cannot expand a stored target, `mt path list` lists that ID in `unresolved_targets` and counts progress over the remaining targets. Other commands for that path fail with `unknown id`.

## Atom lifecycle

1. **Untaught.** The atom exists in the curriculum without user overlay content.
2. **Lesson stored.** A lesson body exists in the curriculum or is saved to the overlay via `mt lesson upsert`.
3. **Lesson taught.** The path event log contains `lesson_taught` or `lesson_authored` for the atom.
4. **Quiz stored.** For each difficulty slot, the agent saves a quiz via `mt quiz create` if prompted by `create_quiz`.
5. **Quiz answered.** If `mt path next` returns `present_quiz`, the agent presents the question, grades the user response, and records the result with `mt quiz answer`.

## `mt path next` output

`mt path next` writes a single AYML record to stdout:
- Under the `bottom_up` strategy, `mt path next` only selects an atom after all of its prerequisites are complete.
- Under the `top_down` strategy, targets are presented before their prerequisites.

```yaml
schema_version: 1
action: create_lesson | present_lesson | create_quiz | present_quiz | no_reviews_due | done
path: <path-id>
next_step: <command that performs the action>
payload: ...
```

`next_step` includes the path ID so that the agent acts on the same path. `mt mcp` formats `next_step` as an MCP tool call, such as `answer_quiz { quiz_id: "...", path_id: "...", rating: "again|hard|good|easy", user_answer: "<verbatim reply>" }`. The `done` action omits `next_step`.

### `create_lesson`

The selected atom does not have a lesson in the merged graph. The agent authors a lesson, saves it with `mt lesson upsert`, and presents it to the user.

```yaml
action: create_lesson
path: p_2026_05_09_173042_k3x9
next_step: "mt lesson upsert la.5.4.7 --path p_2026_05_09_173042_k3x9 --body '<lesson>'"
payload:
  atom:
    id: la.5.4.7
    name: "Singular values from A*A eigenvalues"
    description: "σᵢ² are the non-zero eigenvalues of A* A."
  prerequisites:
    - id: la.5.4.2
      name: "Singular value"
      description: "Diagonal entry of Σ; non-negative real number."
```

### `present_lesson`

A lesson exists in the merged graph, but it has not been taught on this path. `mt path next` appends a `lesson_taught` event if it emits this action. The agent presents the lesson and then calls `mt path next`.

```yaml
action: present_lesson
path: p_2026_05_09_173042_k3x9
next_step: "mt path next --path p_2026_05_09_173042_k3x9"
payload:
  atom:
    id: la.5.4.7
    name: "Singular values from A*A eigenvalues"
    description: "σᵢ² are the non-zero eigenvalues of A* A."
    lesson: """...stored body..."""
  reason: not_taught
  history:
    repetitions: 0
```

### `create_quiz`

The lesson has been taught on this path, but a difficulty slot does not have a quiz. The agent authors a quiz, saves it with `mt quiz create`, presents it to the user, and records the grade with `mt quiz answer`.

```yaml
action: create_quiz
path: p_2026_05_09_173042_k3x9
next_step: "mt quiz create la.5.4.7 --path p_2026_05_09_173042_k3x9 --difficulty medium --question '<question>' --answer '<answer>' [--rubric '<rubric>']"
payload:
  atom:
    id: la.5.4.7
    name: "Singular values from A*A eigenvalues"
    description: "σᵢ² are the non-zero eigenvalues of A* A."
    lesson: """..."""
  target_difficulty: medium
  existing_quizzes:
    - id: la.5.4.7.q1
      difficulty: easy
      question: """..."""
  prerequisites:
    - id: la.5.4.2
      name: "Singular value"
      description: "..."
```

### `present_quiz`

A quiz card is due or required for atom completion. The agent presents the question, grades the user response against the reference answer and rubric, and records the FSRS rating with `mt quiz answer`.

```yaml
action: present_quiz
path: p_2026_05_09_173042_k3x9
next_step: "mt quiz answer la.5.4.7.q2 --path p_2026_05_09_173042_k3x9 --rating <again|hard|good|easy> --user-answer '<verbatim reply>'"
payload:
  atom:
    id: la.5.4.7
    name: "..."
    description: "..."
    lesson: """..."""
  quiz:
    id: la.5.4.7.q2
    difficulty: medium
    type: free_text
    question: """...stored question..."""
    answer:   """...reference answer..."""
    rubric:   """...optional grading guidance..."""
  history:
    repetitions: 5
    last_presented_at: 2026-05-08T14:23:11Z
    correct_count: 3
    total_count: 5
    correct_pct: 60
    recent_ratings: [easy, good, hard, again, good]
```

### `no_reviews_due`

`mt path next --due` returns this action if zero review cards are due. The path can still be incomplete. `next_step` is `mt path next` without `--due`.

```yaml
action: no_reviews_due
path: p_2026_05_09_173042_k3x9
next_step: "mt path next --path p_2026_05_09_173042_k3x9"
payload:
  message: "Zero reviews are due."
```

### `done`

All target atoms on the path are complete, and zero review cards are currently due.

```yaml
action: done
path: p_2026_05_09_173042_k3x9
payload:
  message: "Path complete."
```

## `mt path syllabus` output

`mt path syllabus` lists upcoming atoms with untaught lessons in scheduled teaching order. The output omits lesson bodies. An atom leaves the syllabus as soon as its lesson is taught, even if its quizzes are incomplete.

```yaml
schema_version: 1
path: p_2026_05_09_173042_k3x9
goal: "Understand SVD"
total_remaining: 23
atoms:
  - id: la.5.4.7
    name: "Singular values from A*A eigenvalues"
    description: "σᵢ² are the non-zero eigenvalues of A* A."
```

## Storage

The curriculum graph is embedded into the binary at build time from `curriculum/graph/`. Pass `--graph DIR` before the command, as in `mt --graph DIR path next`, or set `MT_GRAPH` to load an external curriculum directory instead. Every command of the process, including `mt mcp`, uses this curriculum. `mt graph check -p DIR` validates the files in `DIR`.

User state is stored in a Baserunna database configured via `BASERUNNA_DB`, `BASERUNNA_URL`, and `BASERUNNA_TOKEN`. `BASERUNNA_DB` defaults to `mathtutor`, and `BASERUNNA_URL` defaults to `https://baserunna.fly.dev`.

Database setup and key checks follow these rules:

- **Rejected or restricted keys:** If the server rejects `BASERUNNA_TOKEN`, the command reports the server URL and exits with code `2`. If the key is limited to another database or is read-only during a write, the command reports the database, the server, and the key limits to check, and exits with code `2`.
- **Database creation:** Only `mt path new` and `mt import-turso` create a missing database, and each reports the creation. Other commands fail with exit code `2` if the database does not exist, so a wrong `BASERUNNA_DB` cannot start a second learner history.
- **Table creation:** Every command that reads or writes learner state creates the tables in an empty existing database, such as a console-created database. If table creation stops early, the next command finishes it. A command fails with exit code `2` if the database holds a table from another application.
- **MCP startup:** `mt mcp` starts without a database request. If the database exists without tables, the first tool call creates the learner tables and the first OAuth request creates the authorization tables. `/ready` reports a missing or unusable database.

| Data                   | Storage type          | Purpose                                                            |
| ---------------------- | --------------------- | ------------------------------------------------------------------ |
| `paths`                | Table, by path ID     | Path goal, strategy, target IDs, and active subpath                |
| `events.<path id>`     | Stream                | Append-only learning history for a path                            |
| `cards`                | Table, by path + quiz | Derived FSRS card state and answer statistics                      |
| `taught`               | Table, by path + atom | Derived set of lessons taught on a path                            |
| `lessons`              | Table, by atom ID     | User-authored lesson bodies in the overlay                         |
| `quizzes`              | Table, by quiz ID     | User-authored or updated quizzes in the overlay                    |
| `removed_quizzes`      | Table, by quiz ID     | Deleted quiz tombstones in the overlay                             |
| `activity/<path id>`   | Key                   | Timestamp of the latest write for a path                           |
| `overlay_version`      | Key                   | Random token replaced on every overlay write                       |
| `mathtutor_state`      | Key                   | Schema version of the learner tables and the progress of an import |
| `mathtutor_auth_state` | Key                   | Schema version of the authorization tables of `mt mcp`             |

The learner tables and the authorization tables have separate schema versions. The authorization tables are the OAuth tables in `docs/mcp.md`. Only `mt mcp` and `mt import-turso` create them, so an authorization schema change does not stop the CLI on a device. If the stored authorization tables have an older version, the first OAuth request drops and recreates them while preserving `oauth_clients` if its definition matches the new release. Connectors must sign in again without re-registering, and learner data stays unchanged. A server rejects tables written by a newer release. A test pins a fingerprint of each table set to its version so that any table change requires a version bump.

Reads and writes follow these rules:

- **Transactions:** Each write command executes as a serializable transaction that reads a snapshot and commits with optimistic concurrency control. Read commands read the latest state without a transaction.
- **State checks:** Each request reads `mathtutor_state` and fails if the database has a different learner schema version or an open import. Each OAuth request and access-token check also reads `mathtutor_auth_state`, whereas `MT_API_KEY` tool calls skip it.
- **Most recent path:** A command without `--path` selects the most recent path before its first attempt. The most recent path is the path whose activity key was written by the latest commit on the server.
- **Retries and unknown outcomes:** Write conflicts retry the transaction. If a commit response is lost, the client queries Baserunna for the commit status and exits with code `3` if the status is unavailable.
- **Overlay cache:** The client caches the overlay in memory and under `$MATHTUTOR_HOME`, which defaults to `~/.mathtutor`. A command reads the cache file only if it needs the overlay, and `mt mcp` caches the overlay only in memory. The client loads overlay rows before a write transaction, reads only `overlay_version` inside the transaction, and restarts if `overlay_version` changes.

`mt import-turso` migrates a libSQL database into an empty Baserunna database. It reads the libSQL URL from `--url` or `TURSO_URL`, and it sends `TURSO_AUTH_TOKEN` as the libSQL auth token if the variable is set.

The import reads a consistent snapshot and writes the overlay and the OAuth clients. It writes each path in a single transaction with its events and the derived `cards` and `taught` rows. Next, it reads the source again and verifies that every source row, path, event, answered card, and overlay entry matches the stored data. Decoding rejects any source field that the import cannot store. Reads and writes of learner state are blocked while an import is open. An import with mismatches stays open so that it can resume, or the operator can delete the database and rerun the import. Once an import completes with zero mismatches, subsequent runs are rejected.

### Baserunna keys

A Baserunna API key can be restricted to a single database and to read-only access. Each key acts with the role of the user who created it and never expires. A database-limited key cannot create or delete a database, whereas an unrestricted key can delete every database owned by its user. Only a signed-in user in the web console can create or delete keys.

Use this setup so that a leaked key cannot delete the learner history:

1. Create the database. Either click "Create database" in the web console, or use a temporary unrestricted key for the first `mt path new` or `mt import-turso`.
2. Create a database-limited key for the MCP server and set it as `BASERUNNA_TOKEN` on the server.
3. Create a separate database-limited key for each device. Delete only that device's key to retire the device or recover from a leak.
4. Delete the temporary unrestricted key if you created it.

Keep these keys writable, because every teaching command writes events. A database-limited writer key can still drop tables and delete or trim event streams, and Baserunna lacks backups; `TODO.md` tracks both issues.

## User overlay

User-authored lessons and quizzes are stored in the database overlay and shared across all paths:

- **Lessons:** An entry in `lessons` overrides the curriculum lesson for that atom.
- **Quizzes:** Entries in `quizzes` replace curriculum quizzes with matching IDs or append new quizzes. Any quiz ID present in `removed_quizzes` is excluded.
- **Quiz IDs:** Curriculum quizzes use `<atom>.q<N>`, and `mt quiz create` assigns `<atom>.u<N>` to user quizzes so that later curriculum releases cannot collide with user quizzes. Each overlay row stores its atom ID directly. Quizzes copied by `mt import-turso` keep their `<atom>.q<N>` IDs.
- **Topology:** Prerequisites and cluster hierarchy are defined exclusively by the curriculum files.

## Event log

Each path records learning activity in an append-only `events.<path id>` stream with UTC RFC 3339 timestamps. `cards` and `taught` are materialized projections of the event stream. Each event is a JSON object with `ts`, `path`, `kind`, and the fields of its kind:

```json
{
  "ts": "2026-05-09T18:42:01.000Z",
  "path": "<path-id>",
  "kind": "quiz_answered",
  "atom": "la.5.4.7",
  "quiz": "la.5.4.7.q1",
  "rating": "good",
  "user_answer": "..."
}
```

| Kind                                                              | Fields                                                    |
| ----------------------------------------------------------------- | --------------------------------------------------------- |
| `path_created`                                                    | None                                                      |
| `lesson_authored`, `lesson_amended`, `lesson_taught`              | `atom`                                                    |
| `quiz_authored`, `quiz_presented`, `quiz_amended`, `quiz_removed` | `atom`, `quiz`                                            |
| `quiz_answered`                                                   | `quiz`, `rating`, and optionally `atom` and `user_answer` |

A read rejects an event that is missing a required field or contains an unexpected field for its kind.

## Spaced repetition

Each quiz on a path has an FSRS card whose state is derived from its `quiz_answered` events.

`mt path next` selects actions in the following order:

1. If a quiz card in the path is due for review and `--new` is omitted, return `present_quiz` for the earliest-due card.
2. Otherwise, select the first incomplete atom in the active strategy order and return the first applicable action:
   - `create_lesson` if the atom does not have a lesson.
   - `present_lesson` if the lesson has not been taught on this path.
   - `create_quiz` if the `easy`, `medium`, or `hard` difficulty slot does not have a quiz.
   - `present_quiz` if a quiz has not been answered with `hard`, `good`, or `easy`.
3. Otherwise, return `done`.

`--due` skips step 2 and returns `no_reviews_due` if zero cards are due.

An atom is complete if its lesson is taught and each difficulty slot has at least a single quiz answered with `hard`, `good`, or `easy`. An `again` rating does not complete a slot. An unpassed quiz is presented again immediately, whereas a failed review of a previously passed quiz is scheduled at its next FSRS due time.

## Teaching strategies

A path uses either the `bottom_up` or `top_down` traversal strategy. `mt path strategy` changes the active strategy at any time.

- **Bottom-up**: Default strategy. Traverses the prerequisite DAG in post-order DFS so that every prerequisite is completed before its dependents.
- **Top-down**: Presents the target atoms without non-target prerequisites. If a target atom requires another target atom, the required atom comes first.

### Subpaths

If a user is blocked on a target atom in `top_down` mode, the agent can set a subpath with `mt path subpath set` that lists prerequisite atoms leading to that target. While a subpath is active in `top_down` mode, `mt path next` teaches the incomplete atoms in the subpath in order. Once the final atom in the subpath is completed, the subpath is cleared automatically.

## Tool I/O and exit codes

- Standard output: A single AYML record for:
  - `mt path next`
  - `mt path list`
  - `mt path syllabus`
  - `mt graph show`
  - `mt graph list`
  - `mt graph dump`
  `mt path state` and `mt path tree` print text for people to read.
- Standard error: Confirmation messages from write commands, including the ID of a new quiz or path.
- Standard error: Diagnostic and error messages.
- Exit codes: `0` for success, `1` for runtime or state errors, `2` for usage or validation errors, and `3` if the commit status is unknown. After exit `3`, the command prints its safe next step. If the reader closes stdout early, as `head` does, a command that otherwise succeeds exits with code `0` without printing a message.

Each error has a kind that sets its exit code. An MCP tool error reports the same kind in its `kind` field.

| Kind                      | Exit code | Cause                                                    |
| ------------------------- | --------- | -------------------------------------------------------- |
| `unknown_id`              | 2         | The curriculum does not contain the ID.                  |
| `empty_cluster`           | 2         | A cluster target has zero atoms.                         |
| `no_targets`              | 2         | The new path has zero targets.                           |
| `cycle`                   | 2         | The target atoms form a prerequisite cycle.              |
| `not_atom`                | 2         | The ID specifies a cluster where an atom is required.    |
| `subpath_not_top_down`    | 2         | The path does not use the `top_down` strategy.           |
| `subpath_empty`           | 2         | The subpath has zero atoms.                              |
| `subpath_tail_not_target` | 2         | The last subpath atom is not a path target.              |
| `subpath_duplicate_atom`  | 2         | The subpath lists an atom more than once.                |
| `conflicting_flags`       | 2         | The command has incompatible flags.                      |
| `config`                  | 2         | A setting such as `BASERUNNA_TOKEN` is missing or wrong. |
| `no_path`                 | 1         | The learning path does not exist.                        |
| `no_lesson`               | 1         | The atom does not have a stored lesson.                  |
| `io`                      | 1         | A file read, file write, or AYML conversion failed.      |
| `graph_check`             | 1         | The curriculum check found problems.                     |
| `corrupt`                 | 1         | `mt` cannot decode a stored row or server response.      |
| `database`                | 1         | A database request failed, or the schema version differs.|
| `json`                    | 1         | A JSON conversion failed.                                |
| `fsrs`                    | 1         | The FSRS scheduler failed.                               |
| `import`                  | 1         | An import failed, or an import is in progress.           |
| `http`                    | 1         | `mt` cannot start an HTTP client.                        |
| `server`                  | 1         | `mt mcp` cannot bind or serve its address.               |
| `internal`                | 1         | `mt` encountered an internal invariant violation.        |
| `outcome_unknown`         | 3         | The commit status is unknown.                            |
