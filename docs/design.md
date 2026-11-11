# Math Tutor Design

Math Tutor manages math learning through a directed acyclic graph (DAG) of lessons and quizzes with spaced repetition. The curriculum graph is compiled into the `mt` binary. User state is stored in a [Baserunna](https://baserunna.fly.dev) database. Curriculum files and CLI output use [AYML](https://crates.io/crates/ayml).

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
mt path new <GOAL> --targets <ID>[,<ID>...] [--strategy {bottom-up,top-down}]
mt path state [--path P]
mt path next  [--path P]
mt path strategy {bottom-up,top-down} [--path P]
mt path subpath set --atoms <ID>[,<ID>...] [--path P]
mt path subpath clear [--path P]
mt path syllabus [--path P] [-n N]
mt path tree  [--path P] [--depth N]

# Curriculum lookup (read-only)
mt graph list  [<ID>] [--path P] [--recursive]
mt graph show  <ID>   [--path P]
mt graph check [-p DIR]
mt graph dump

# Authoring (writes to the user overlay)
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

If `--path P` is provided to `mt graph show` or `mt graph list`, the output includes the `lesson_taught` and `complete` fields for that path. `mt graph list --recursive` also lists every descendant of the node.

### Target ID resolution

Each `--targets` entry is either an atom ID, such as `tx.1.1`, or a cluster ID, such as `tx.1`. A cluster ID expands to all of its atomic descendants.

`mt path new` validates that each entry expands to at least a single atom and stores the raw IDs in the `paths` table. Target IDs and subpath IDs are expanded to a deduplicated, topologically sorted list of atoms each time the path is loaded. If an atom is later split into a cluster in the curriculum, the stored ID expands to the new descendant atoms.

## Atom lifecycle

1. **Untaught.** The atom exists in the curriculum without user overlay content.
2. **Lesson stored.** A lesson body exists in the curriculum or is saved to the overlay via `mt lesson upsert`.
3. **Lesson taught.** The path event log contains `lesson_taught` or `lesson_authored` for the atom.
4. **Quiz stored.** For each difficulty slot, the agent saves a quiz via `mt quiz create` if prompted by `create_quiz`.
5. **Quiz answered.** If `mt path next` returns `present_quiz`, the agent presents the question, grades the user response, and records the result with `mt quiz answer`.

## `mt path next` output

`mt path next` writes a single AYML record to stdout:
- Under the `bottom-up` strategy, `mt path next` only selects an atom after all of its prerequisites are complete.
- Under the `top-down` strategy, targets are presented before their prerequisites.

```yaml
schema_version: 1
action: create_lesson | present_lesson | create_quiz | present_quiz | done
path: <path-id>
next_step: <command that performs the action>
payload: ...
```

`next_step` names the path explicitly, so that the agent acts on the same path. `mt mcp` returns `next_step` as an MCP tool call, such as `answer_quiz { quiz_id: "...", path_id: "...", rating: "again|hard|good|easy", answer: "<verbatim reply>" }`. The `done` action omits `next_step`.

### `create_lesson`

The selected atom does not have a lesson in the merged graph. The agent authors a lesson, saves it with `mt lesson upsert`, and presents it to the user.

```yaml
action: create_lesson
path: p_2026_05_09_173_42
next_step: "mt lesson upsert la.5.4.7 --path p_2026_05_09_173_42 --body TEXT"
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
path: p_2026_05_09_173_42
next_step: "mt path next --path p_2026_05_09_173_42"
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
path: p_2026_05_09_173_42
next_step: "mt quiz create la.5.4.7 --path p_2026_05_09_173_42 --difficulty medium --question TEXT --answer TEXT [--rubric TEXT]"
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
path: p_2026_05_09_173_42
next_step: "mt quiz answer la.5.4.7.q2 --path p_2026_05_09_173_42 --rating {again|hard|good|easy} --user-answer \"<verbatim reply>\""
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

### `done`

All target atoms on the path are complete, and zero review cards are currently due.

```yaml
action: done
path: p_2026_05_09_173_42
payload:
  message: "Path complete."
```

## `mt path syllabus` output

`mt path syllabus` lists upcoming atoms with untaught lessons in scheduled teaching order. The output omits lesson bodies. An atom leaves the syllabus as soon as its lesson is taught, even if its quizzes are incomplete.

```yaml
schema_version: 1
path: p_2026_05_09_173_42
goal: "Understand SVD"
total_remaining: 23
atoms:
  - id: la.5.4.7
    name: "Singular values from A*A eigenvalues"
    description: "σᵢ² are the non-zero eigenvalues of A* A."
```

## Storage

The curriculum graph is embedded into the binary at build time from `curriculum/graph/`. To load an external curriculum directory instead, pass `--graph DIR` before the command, as in `mt --graph DIR path next`, or set `MT_GRAPH`. Every command of the process, including `mt mcp`, uses this curriculum. `mt graph check -p DIR` validates the files in `DIR`.

User state is stored in a Baserunna database configured via `BASERUNNA_DB`, `BASERUNNA_URL`, and `BASERUNNA_TOKEN`. `BASERUNNA_DB` defaults to `mathtutor`, and `BASERUNNA_URL` defaults to `https://baserunna.fly.dev`. Only `mt path new`, `mt import-turso`, and `mt mcp` create a missing database, and each reports the creation. Other commands fail with exit code `2` if the database does not exist, so a wrong `BASERUNNA_DB` cannot start a second learner history.

| Data                 | Storage type             | Purpose                                                   |
| -------------------- | ------------------------ | --------------------------------------------------------- |
| `paths`              | Table, keyed by path ID  | Path goal, strategy, target IDs, and active subpath       |
| `events.<path id>`   | Stream                   | Append-only learning history for a path                   |
| `cards`              | Table, by `(path, quiz)` | Derived FSRS card state and answer statistics             |
| `taught`             | Table, by `(path, atom)` | Derived set of lessons taught on a path                   |
| `lessons`            | Table, by atom ID        | User-authored lesson bodies in the overlay                |
| `quizzes`            | Table, by quiz ID        | User-authored or updated quizzes in the overlay           |
| `removed_quizzes`    | Table, by quiz ID        | Deleted quiz tombstones in the overlay                    |
| `activity/<path id>` | Key                      | Timestamp of the latest write for a path                  |
| `overlay_version`    | Key                      | Random token replaced on every overlay write              |

Each write command executes as a serializable transaction that reads a snapshot and commits with optimistic concurrency control. Read commands read the latest state without a transaction. A command without `--path` selects the most recent path once, before its first attempt. If a write conflict occurs, the transaction is retried. If a commit response is lost, the client queries Baserunna for the commit status. If the status is unavailable, the command exits with code `3` because its outcome is unknown. The client caches the overlay in memory and under `$MATHTUTOR_HOME`, which defaults to `~/.mathtutor`. It reloads the overlay only if `overlay_version` changes. A write transaction reads only `overlay_version`. The client loads the overlay rows before the transaction and restarts the transaction if the version changes, so the overlay size does not count against the 16 MB read limit of a transaction.

`mt import-turso` migrates a libSQL database into an empty Baserunna database. It reads a consistent snapshot, writes the paths, overlay, events, and OAuth clients, and rebuilds `cards` and `taught` from the event streams. It then verifies the paths, overlay, events, and cards against a fresh read of the source rows. While an import is open, reads and writes of learner state are blocked. An import with mismatches stays open so that it can be resumed. If mismatches remain, delete the database and run the import again. Once an import completes with zero mismatches, subsequent runs are rejected to preserve newer data.

## User overlay

User-authored lessons and quizzes are stored in the database overlay and shared across all paths:

- **Lessons:** An entry in `lessons` overrides the curriculum lesson for that atom.
- **Quizzes:** Entries in `quizzes` replace curriculum quizzes with matching IDs or append new quizzes. Any quiz ID present in `removed_quizzes` is excluded.
- **Topology:** Prerequisites and cluster hierarchy are defined exclusively by the curriculum files.

## Event log

Each path records learning activity in an append-only `events.<path id>` stream with UTC RFC 3339 timestamps. `cards` and `taught` are materialized projections of the event stream.

```yaml
- ts: 2026-05-09T18:42:01Z
  type: quiz_answered
  path: <path-id>
  atom: la.5.4.7
  quiz: la.5.4.7.q1
  payload:
    rating: good
    user_answer: """..."""
```

Event types:

- `path_created`
- `lesson_authored`
- `lesson_amended`
- `lesson_taught`
- `quiz_authored`
- `quiz_presented`
- `quiz_answered`
- `quiz_amended`
- `quiz_removed`

## Spaced repetition (FSRS)

Each quiz on a path has an FSRS card whose state is derived from its `quiz_answered` events.

`mt path next` selects actions in the following order:

1. If a quiz card in the path is due for review and `--new` is omitted, return `present_quiz` for the earliest-due card.
2. Otherwise, select the first incomplete atom in the active strategy order and return the first applicable action:
   - `create_lesson` if the atom does not have a lesson.
   - `present_lesson` if the lesson has not been taught on this path.
   - `create_quiz` if the `easy`, `medium`, or `hard` difficulty slot does not have a quiz.
   - `present_quiz` if a quiz has not been answered with `hard`, `good`, or `easy`.
3. Otherwise, return `done`.

An atom is complete if its lesson is taught and each difficulty slot has at least a single quiz answered with `hard`, `good`, or `easy`. An `again` rating does not complete a slot. An unpassed quiz is presented again immediately, whereas a failed review of a previously passed quiz is scheduled at its next FSRS due time.

## Teaching strategies

A path uses either the `bottom-up` or `top-down` traversal strategy. `mt path strategy` changes the active strategy at any time.

- **Bottom-up**: Default strategy. Traverses the prerequisite DAG in post-order DFS so that every prerequisite is completed before its dependents.
- **Top-down**: Presents target atoms directly in target order without requiring prerequisites first.

### Subpaths

If a user is blocked on a target atom in `top-down` mode, the agent can set a subpath with `mt path subpath set` that lists prerequisite atoms leading to that target. While a subpath is active in `top-down` mode, `mt path next` teaches the incomplete atoms in the subpath in order. Once the final atom in the subpath is completed, the subpath is cleared automatically.

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
- Exit codes: `0` for success, `1` for runtime or state errors, `2` for usage or validation errors, and `3` if the commit status is unknown. After exit `3`, the command prints its safe next step.
