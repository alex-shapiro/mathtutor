# Math Tutor Design

Math Tutor manages math learning through a directed acyclic graph (DAG) of lessons and quizzes with spaced repetition. The curriculum graph is compiled into the `mt` binary, and user state is stored in a [Baserunna](https://baserunna.fly.dev) database. Curriculum files and CLI output use [AYML](https://crates.io/crates/ayml).

## Responsibilities

`mt` is invoked by an LLM agent:

- `mt` manages scheduling, persistence, graph validation, and the user overlay.
- The agent authors lessons and quizzes, presents material to the user, and grades responses.

`mt path next` selects the next action, and the agent presents the material. When a lesson or quiz is required for the first time, the agent authors it and saves it with `mt lesson upsert` or `mt quiz create`. All authored content is written to the user overlay so that subsequent presentations are deterministic.

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
mt path tree  [--path P]

# Curriculum lookup (read-only)
mt graph list  [<ID>] [--path P]
mt graph show  <ID>   [--path P]
mt graph check
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

When `--path P` is provided to `mt graph show` or `mt graph list`, the output includes per-path completion fields (`lesson_taught`, `complete`).

### Target ID resolution

Each `--targets` entry is either an atom ID (a leaf node, such as `tx.1.1`) or a cluster ID (a non-leaf node, such as `tx.1`, which expands to all atomic descendants).

`mt path new` validates that each entry expands to at least a single atom and stores the raw IDs in the `paths` table. Target IDs and subpath IDs are expanded to a deduplicated, topologically sorted list of atoms whenever the path is loaded. If an atom is later split into a cluster in the curriculum, the stored ID is expanded to the new descendant atoms.

## Atom lifecycle

1. **Untaught.** The atom exists in the curriculum without user overlay content.
2. **Lesson stored.** A lesson body exists in the curriculum or is saved to the overlay via `mt lesson upsert`.
3. **Lesson taught.** The path event log contains `lesson_taught` or `lesson_authored` for the atom.
4. **Quiz stored.** For each difficulty slot (`easy`, `medium`, `hard`), the agent saves a quiz via `mt quiz create` when prompted by `create_quiz`.
5. **Quiz answered.** When `mt path next` returns `present_quiz`, the agent presents the question, grades the user response, and records the result with `mt quiz answer`.

## `mt path next` output

`mt path next` writes a single AYML record to stdout:

```yaml
schema_version: 1
action: create_lesson | present_lesson | create_quiz | present_quiz | done
path: <path-id>
payload: ...
```

Under the `bottom-up` strategy, `mt path next` only selects an atom after all of its prerequisites are complete. Under the `top-down` strategy, targets are presented before their prerequisites.

### `create_lesson`

The selected atom does not have a lesson in the merged graph. The agent authors a lesson, saves it with `mt lesson upsert`, and presents it to the user.

```yaml
action: create_lesson
path: p_2026_05_09_173_42
payload:
  atom:
    id: la.5.4.7
    name: "Singular values from A*A eigenvalues"
    description: "σᵢ² are the non-zero eigenvalues of A* A."
  prerequisites:
    - id: la.5.4.2
      name: "Singular value"
      description: "Diagonal entry of Σ; non-negative real number."
      lesson: """..."""
  next_step: "mt lesson upsert la.5.4.7 --body TEXT"
```

### `present_lesson`

A lesson exists in the merged graph, but it has not been taught on this path. `mt path next` appends a `lesson_taught` event when it emits this action. The agent presents the lesson and then calls `mt path next`.

```yaml
action: present_lesson
path: p_2026_05_09_173_42
payload:
  atom:
    id: la.5.4.7
    name: "Singular values from A*A eigenvalues"
    description: "σᵢ² are the non-zero eigenvalues of A* A."
    lesson: """...stored body..."""
  reason: not_taught
  history:
    repetitions: 0
  next_step: "mt path next"
```

### `create_quiz`

The lesson has been taught on this path, but a difficulty slot does not have a quiz. The agent authors a quiz, saves it with `mt quiz create`, presents it to the user, and records the grade with `mt quiz answer`.

```yaml
action: create_quiz
path: p_2026_05_09_173_42
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
      lesson: """..."""
  next_step: "mt quiz create la.5.4.7 --difficulty medium --question TEXT --answer TEXT [--rubric TEXT]"
```

### `present_quiz`

A quiz card is due or required for atom completion. The agent presents the question, grades the user response against the reference answer and rubric, and records the FSRS rating with `mt quiz answer`.

```yaml
action: present_quiz
path: p_2026_05_09_173_42
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
  next_step: "mt quiz answer la.5.4.7.q2 --rating {again|hard|good|easy}"
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

`mt path syllabus` lists upcoming atoms with untaught lessons in scheduled teaching order. Lesson bodies are omitted.

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

An atom is removed from the syllabus as soon as its lesson is taught, even if its quizzes are incomplete.

## Storage

The curriculum graph is embedded into the binary at build time from `curriculum/graph/`. Use `--graph DIR` or `MT_GRAPH` to load an external curriculum directory instead.

User state is stored in a Baserunna database configured via `BASERUNNA_DB` (default `mathtutor`), `BASERUNNA_URL` (default `https://baserunna.fly.dev`), and `BASERUNNA_TOKEN`.

| Data                 | Storage type             | Purpose                                                   |
| -------------------- | ------------------------ | --------------------------------------------------------- |
| `paths`              | Table, keyed by path ID  | Path goal, strategy, target IDs, and active subpath       |
| `events.<path id>`   | Stream                   | Append-only learning history for a path                   |
| `cards`              | Table, by `(path, quiz)` | Derived FSRS card state and answer statistics             |
| `taught`             | Table, by `(path, atom)` | Derived set of lessons taught on a path                   |
| `lessons`            | Table, by atom ID        | User-authored lesson bodies (overlay)                     |
| `quizzes`            | Table, by quiz ID        | User-authored or updated quizzes (overlay)                |
| `removed_quizzes`    | Table, by quiz ID        | Deleted quiz tombstones (overlay)                         |
| `activity/<path id>` | Key                      | Timestamp of the latest event on a path                   |
| `overlay_version`    | Key                      | Monotonic counter incremented on every overlay write      |

Each command executes as a serializable transaction that reads state in a snapshot and commits writes with optimistic concurrency control. If a write conflict occurs, the transaction is retried. The client caches the overlay in memory and under `$MATHTUTOR_HOME` (default `~/.mathtutor`), and reloads it only when `overlay_version` changes.

`mt import-turso` migrates an existing libSQL database to Baserunna by reading a consistent snapshot, writing the paths, overlay, and events, rebuilding `cards` and `taught` from the event streams, and verifying the rebuilt cards against the source rows.

## User overlay

User-authored lessons and quizzes are stored in the database overlay and shared across all paths:

- **Lessons:** An entry in `lessons` overrides the curriculum lesson for that atom.
- **Quizzes:** Entries in `quizzes` replace curriculum quizzes with matching IDs or append new quizzes. Any quiz ID present in `removed_quizzes` is excluded.
- **Topology:** Prerequisites and cluster hierarchy are defined exclusively by the curriculum files.

## Event log

Each path records learning activity in an append-only stream (`events.<path id>`) with UTC RFC 3339 timestamps:

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

`cards` and `taught` are materialized projections of the event stream.

## Spaced repetition (FSRS)

Each quiz on a path has an FSRS card whose state is derived from its `quiz_answered` events.

`mt path next` selects actions in the following order:

1. If a quiz card in the path is due for review (and `--new` is not set), return `present_quiz` for the earliest-due card.
2. Otherwise, select the first incomplete atom in the active strategy order and return the first applicable action:
   - `create_lesson` if the atom does not have a lesson.
   - `present_lesson` if the lesson has not been taught on this path.
   - `create_quiz` if the `easy`, `medium`, or `hard` difficulty slot does not have a quiz.
   - `present_quiz` if a quiz has not been answered with `hard`, `good`, or `easy`.
3. Otherwise, return `done`.

An atom is complete when its lesson is taught and each difficulty slot (`easy`, `medium`, `hard`) has at least a single quiz answered with `hard`, `good`, or `easy`. An `again` rating does not complete a slot and causes the quiz to be presented again immediately.

## Teaching strategies

A path uses either the `bottom-up` or `top-down` traversal strategy. `mt path strategy` changes the active strategy at any time.

- **Bottom-up** (default): Traverses the prerequisite DAG in post-order DFS so that every prerequisite is completed before its dependents.
- **Top-down**: Presents target atoms directly in target order without requiring prerequisites first.

### Subpaths

When a user is blocked on a target atom in `top-down` mode, the agent can set a subpath (`mt path subpath set --atoms a,b,...,<target>`) that lists prerequisite atoms leading to that target. While a subpath is active in `top-down` mode, `mt path next` teaches the incomplete atoms in the subpath in order. When the final atom in the subpath is completed, the subpath is cleared automatically.

## Tool I/O and exit codes

- Standard output: A single AYML record per command.
- Standard error: Diagnostic and error messages.
- Exit codes: `0` for success, `1` for runtime or state errors, `2` for usage or validation errors.
