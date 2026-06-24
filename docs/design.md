# Math Tutor

Math Tutor is a tool for learning math via a DAG of small lessons and
quizzes. It incorporates spaced repetition so that learned concepts stay
learned. It runs as a CLI: the curriculum graph is compiled into the
binary and per-user state lives in a local libSQL (SQLite) database under
`~/.mathtutor/`. Curriculum source and tool I/O use
[AYML](https://crates.io/crates/ayml), a safe, serde-compatible variant of
YAML. AYML uses Swift-like triple-quoted multiline strings instead of `|`,
and disallows YAML's long tail of fringe features.

## Roles

`mt` is invoked as a tool from inside an existing LLM agent (e.g. Claude, ChatGPT, Gemini):

- `mt` owns scheduling, persistence, deterministic reuse, graph
  validation, and the user overlay where the user's authored
  content lives.
- The LLM authors lessons and quizzes, presents them to the user,
  and grades free-text answers.

`mt path next` decides _what_ should be presented; the agent decides
_how_ it is presented. When a lesson or quiz is presented for the first
time on a given path, the agent authors it and persists via
`mt lesson upsert` or `mt quiz create`. Every subsequent presentation is
deterministic. `mt lesson upsert` can be used to create or revise a lesson.
Quizzes are revised with `mt quiz update`. All authoring writes to the user
overlay, never to the shipped curriculum.

Each atom is one concept. Lessons are short (1–2 paragraphs, ≤ 2 minutes
reading, ≤ 1 theorem / rule / definition). Quizzes are short, free-text
by default, and depend only on the current lesson plus previously-taught
lessons. No lesson depends on lookahead material.

The binary embeds the agent operator playbook and prints it via `mt instruct`.

## Commands

Subcommands are resource-first (`mt <noun> <verb>`).
Most commands exist in both CLI and MCP interfaces.
Operator-only verbs (`graph check`, `graph dump`, `instruct`,
`migrate-from-ayml`, `mcp`) have no MCP equivalent.

```bash
# Path lifecycle
mt path list                       # list all paths with goal / progress
mt path new <GOAL> --targets <ID>[,<ID>...]  # start a new learning path
                   [--strategy {bottom-up,top-down}]  # initial traversal mode (default bottom-up)
mt path state [--path P]           # one-screen status summary
mt path next  [--path P]           # next scheduled action (AYML on stdout)
mt path strategy {bottom-up,top-down} [--path P]      # switch traversal mode
mt path subpath set --atoms <ID>[,<ID>...] [--path P] # top-down detour ending in a target
mt path subpath clear [--path P]                      # drop the detour
mt path syllabus [--path P] [-n N] # upcoming lesson topics (no bodies; default N=10)
mt path tree  [--path P]           # full reachable-graph progress view

# Curriculum lookup (read-only)
mt graph list  [<ID>] [--path P]   # areas, or children of a cluster
mt graph show  <ID>   [--path P]   # full detail on atom / cluster / area
mt graph check                     # validate the shipped curriculum
mt graph dump                      # print the user overlay AYML (operator-only)

# Authoring (writes to the user overlay, not to shipped data)
mt lesson upsert <ATOM>    --body TEXT                       # upsert by atom id
mt quiz   create <ATOM>    --difficulty D \
                           --question TEXT --answer TEXT [--rubric TEXT] \
                           [--type {free_text,multiple_choice}]
mt quiz   update <QUIZ_ID> [--question TEXT] [--answer TEXT] [--rubric TEXT] \
                           [--difficulty D] [--type T]
mt quiz   delete <QUIZ_ID>

# User outcomes
mt quiz answer <QUIZ_ID> --rating {again,hard,good,easy} [--user-answer TEXT]

# Agent operator playbook
mt instruct                        # print AGENTS playbook embedded in binary
```

`mt graph show` and `mt graph list` are pure curriculum lookups by
default. When `--path P` is supplied, atom output is enriched with
per-path status (`lesson_taught`, `complete`) without altering the
base shape. Fields are added via `skip_serializing_if`.

Every command that records learning activity appends a structured event
to the per-path log (see "Event log" below); agents read the log if they
need history.

### `--targets` ID resolution

Each `--targets` entry may be:

- an **atom ID** is a leaf concept (e.g. `tx.1.1`)
- a **non-leaf ID** — a cluster (`tx.1`) or an area root (`tx`) — expands to all its atomic descendants

Mixing forms is allowed. `mt path new` validates that each entry expands to
at least one atom, then stores the entries **verbatim** (deduplicated,
order preserved) in `path_targets`. The stored IDs are expanded to a
deduplicated, topologically sorted set of atoms **on every load**, so the
resolved target set tracks later curriculum edits — e.g. an atom that is
split into a cluster keeps resolving to the right leaves without a data
migration. Resolution errors only if an entry no longer maps to any atom.

Subpaths get the same treatment: `mt path subpath set --atoms` requires
leaf atoms at set time, but the stored sequence is re-expanded on load, so
a subpath atom later split into a cluster resolves to its descendants
(in order) rather than being skipped by the scheduler.

## Lifecycle of an atom (within a path)

1. **Bare in the path.** Atom exists in the shipped curriculum with
   `id`, `name`, `description`, `prerequisites`. The user overlay
   has no entry for it.
2. **Lesson present.** Either the shipped curriculum already has a
   lesson body for the atom, or the agent authors one via
   `mt lesson upsert` (overlay).
3. **Lesson taught.** Once the path's log records `lesson_taught` (or
   the implicitly-equivalent `lesson_authored`) for the atom, the
   scheduler considers the lesson "delivered" for this path and moves
   on to quiz work.
4. **Quiz stored (per slot, lazy).** When `mt path next` returns
   `create_quiz` for a difficulty slot, the agent authors a question,
   reference answer, and optional rubric, then calls `mt quiz create`
   to append to the overlay.
5. **Quiz answered.** `mt path next` returns `present_quiz`. The agent
   presents the stored question, grades the user's reply against the
   reference answer + rubric, and calls `mt quiz answer <quiz-id>
--rating …`.

Each lesson and each individual quiz is generated lazily when the
scheduler first asks for it. An atom may be in a partial state (lesson
exists, only the easy quiz exists).

## `mt path next` I/O

`mt path next` writes one AYML record to stdout. Top-level shape:

```yaml
schema_version: 1
action: create_lesson | present_lesson | create_quiz | present_quiz | done
path: <path-id>
payload: ... # action-specific (see below)
```

Conventions for every payload:

- IDs are stable curriculum-graph identifiers (`la.5.4.7`, `la.5.4.7.q2`).
- Multi-paragraph fields (`lesson`, `question`, `answer`, `rubric`) are
  AYML triple-quoted strings.
- `next_step` tells the agent the canonical command to call after acting on this output.
- Under the bottom-up strategy, all atoms `mt path next` returns satisfy
  the prerequisite invariant: every direct prerequisite already has a
  stored lesson in the merged view. Top-down relaxes this by design,
  presenting a target before its prerequisites (see "Teaching strategy).

### `create_lesson`

The next path-target atom has no stored lesson anywhere (neither in the
shipped curriculum nor the overlay). The agent authors one (1–2
paragraphs, ≤ 1 theorem / rule / definition), persists via
`mt lesson upsert`, and presents to the user.

```yaml
action: create_lesson
path: p_2026_05_09_173_42
payload:
  atom:
    id: la.5.4.7
    name: "Singular values from A*A eigenvalues"
    description: "σᵢ² are the non-zero eigenvalues of A* A."
  prerequisites:                          # direct prereqs, with their stored lessons
    - id: la.5.4.2
      name: "Singular value"
      description: "Diagonal entry of Σ; non-negative real number."
      lesson: """..."""
    - id: la.4.1.1
      name: "Eigenvalue / eigenvector"
      description: "Av = λv with v ≠ 0; λ ∈ 𝔽 is the eigenvalue."
      lesson: """..."""
  next_step: "mt lesson upsert la.5.4.7 --body TEXT"
```

### `present_lesson`

A lesson body already exists for this atom (shipped, or authored under
a previous path), but the current path has never taught it. The
scheduler re-surfaces the stored body so the user gets context before
any quiz. The agent shows the body verbatim, then calls `mt path next` again.
`mt path next` auto-logs `lesson_taught` when it returns this action,
so no explicit "I taught it" command is needed.

```yaml
action: present_lesson
path: p_2026_05_09_173_42
payload:
  atom:
    id: la.5.4.7
    name: "Singular values from A*A eigenvalues"
    description: "σᵢ² are the non-zero eigenvalues of A* A."
    lesson: """...stored body..."""
  reason: not_taught                # the only reason currently emitted
  history:
    repetitions: 0                  # past `lesson_taught` events for this atom in this path
  next_step: "mt path next"
```

### `create_quiz`

The atom has a lesson (shipped or overlay) and the path is taught it,
but a difficulty slot has no quiz yet. The agent authors a free-text
question + reference answer + optional rubric, persists via
`mt quiz create`, then presents to the user. The reply rating is logged
with `mt quiz answer` afterwards.

```yaml
action: create_quiz
path: p_2026_05_09_173_42
payload:
  atom:
    id: la.5.4.7
    name: "Singular values from A*A eigenvalues"
    description: "σᵢ² are the non-zero eigenvalues of A* A."
    lesson: """..."""
  target_difficulty: medium               # easy | medium | hard
  existing_quizzes:                       # avoid duplicates
    - id: la.5.4.7.q1
      difficulty: easy
      question: """..."""
  prerequisites:                          # for context while authoring
    - id: la.5.4.2
      name: "Singular value"
      description: "..."
      lesson: """..."""
  next_step: "mt quiz create la.5.4.7 --difficulty medium --question TEXT --answer TEXT [--rubric TEXT]"
```

### `present_quiz`

A quiz card is due. The agent shows the stored question to the user,
grades the reply against the reference answer + rubric, and reports the
FSRS grade with `mt quiz answer`.

```yaml
action: present_quiz
path: p_2026_05_09_173_42
payload:
  atom:
    id: la.5.4.7
    name: "..."
    description: "..."
    lesson: """..."""                     # included so the agent has full grading context
  quiz:
    id: la.5.4.7.q2
    difficulty: medium
    type: free_text
    question: """...stored question..."""
    answer:   """...reference answer..."""
    rubric:   """...optional grading guidance..."""
  history:
    repetitions: 5                        # past `quiz_presented` events for this quiz
    last_presented_at: 2026-05-08T14:23:11Z
    correct_count: 3                      # ratings in {good, easy}
    total_count: 5
    correct_pct: 60
    recent_ratings: [easy, good, hard, again, good]   # most recent first
  next_step: "mt quiz answer la.5.4.7.q2 --rating {again|hard|good|easy}"
```

### `done`

Every target atom of the path has been taught and has at least one
quiz answered with `good` or `easy` for each difficulty slot. The
path goal is reached (modulo ongoing retention reviews).

```yaml
action: done
path: p_2026_05_09_173_42
payload:
  message: "Path complete."
```

### Errors

Failures are written to stderr; stdout stays a single valid AYML record
or is empty. Exit codes: `0` ok; `1` scheduler / state-read failure;
`2` config / IO / validation.

## `mt path syllabus` I/O

`mt path syllabus` is the read-only counterpart to `mt path next`.
It lists every upcoming atom whose lesson hasn't been taught yet, in scheduler-teach order.
Under bottom-up it walks the path's prerequisite graph; under top-down it lists the
subpath's remaining atoms first (when one is set), then the path's untaught targets.
Lesson bodies are deliberately omitted, as this is a roadmap, not a reader.
The `total_remaining` count is always untruncated.

```yaml
schema_version: 1
path: p_2026_05_09_173_42
goal: "Understand SVD"
total_remaining: 23
atoms:
  - id: la.5.4.7
    name: "Singular values from A*A eigenvalues"
    description: "σᵢ² are the non-zero eigenvalues of A* A."
  - id: la.5.4.8
    name: "..."
    description: "..."
```

An atom drops out of the syllabus the moment its `lesson_taught` (or
equivalent `lesson_authored`) event lands in the log, regardless of any
pending quiz work on the same atom — quizzes are part of the do-iterator
surface, not the lookahead.

## Storage

The curriculum graph is compiled into the binary at build time via
`include_dir!` from AYML source (`curriculum/graph/manifest.ayml` and
`areas/<NN>-<slug>.ayml`). To override the embedded copy and develop
against a working tree, use the `--graph DIR` CLI option or the
`MT_GRAPH` env variable.

All user state lives in a libSQL database at `$MATHTUTOR_HOME/mt.db`.
The default path is `~/.mathtutor/mt.db`). When `TURSO_URL` and `TURSO_AUTH_TOKEN`
are set, the file is an embedded replica synced to a Turso server; otherwise
it is a plain local SQLite file. The schema is versioned by numbered, immutable
migrations.

Tables, by role:

| Table                     | Role                          | Mutability                                                     |
| ------------------------- | ----------------------------- | -------------------------------------------------------------- |
| `paths`                   | per-path goal + strategy      | `goal`/`created_at` fixed at `mt path new`; `strategy` mutable |
| `path_targets`            | target IDs, verbatim          | fixed at `mt path new`; expanded to atoms on load             |
| `path_subpath`            | top-down detour to a target   | replaced by `mt path subpath set`, emptied by `clear`          |
| `events`                  | per-path learning history     | append-only                                                    |
| `cards`                   | FSRS state per `(path, quiz)` | write-through cache, rebuildable from `events`                 |
| `overlay_lessons`         | user-authored lessons         | mutated by `mt lesson upsert`                                  |
| `overlay_quizzes`         | user-authored quizzes         | mutated by `mt quiz {create,update}`                           |
| `overlay_removed_quizzes` | user-authored quiz tombstones | mutated by `mt quiz delete`                                    |

The `events` log is the source of truth for learning history; `cards` is a
derived cache that is rebuilt by replaying the event log.
The authored overlay is keyed by atom / quiz id.

## User overlay

Lessons and quizzes the agent authors are stored as SQL rows that overlay
the shipped curriculum without modifying it. The overlay is global to the
user's database, keyed by atom / quiz id — it is shared across paths, not
scoped to one:

- `overlay_lessons(atom_id, body)` — an authored lesson body for an atom
  the shipped graph left bare, or a revision of a shipped one.
- `overlay_quizzes(atom_id, quiz_id, difficulty, kind, question, answer,
rubric)` — quizzes added by `mt quiz create` or revised by
  `mt quiz update`.
- `overlay_removed_quizzes(quiz_id)` — tombstones; the merge skips these.

Merge semantics (`Graph::load_for_path`): an overlay lesson, quiz, or
tombstone always overrides a shipped item with the same id. Concretely:

- **Lesson:** if the overlay has one, use it; otherwise use the
  shipped lesson if present. `mt lesson upsert` is an upsert; a second
  call replaces the body.
- **Quizzes:** start with shipped, replace any with the same id from
  the overlay (this is how `mt quiz update` works), then append overlay
  quizzes whose ids don't match shipped (added by `mt quiz create`),
  then filter out anything whose id appears in `overlay_removed_quizzes`.
- **Prerequisites / cluster structure:** not overridable. The shape
  of the curriculum is fixed by the shipped graph; the overlay only
  carries content.

`mt graph dump` prints the overlay for review and eventual merge back
into the canonical curriculum.

## Event log

Per-path learning history, appended to the `events` table. Every event has
a `ts` field in RFC 3339 / ISO 8601 format with UTC timezone so the log can
be re-played and `time_since_last` / `repetitions` derived without
additional state. Each event, as a logical record:

```yaml
- ts: 2026-05-09T18:42:01Z   # required, RFC 3339 UTC
  type: quiz_answered
  path: <path-id>
  atom: la.5.4.7
  quiz: la.5.4.7.q1
  payload:
    rating: good
    user_answer: """..."""
```

Event kinds:

- `path_created`
- `lesson_authored` (on `mt lesson upsert` when no lesson existed)
- `lesson_amended` (on `mt lesson upsert` when a lesson already existed)
- `lesson_taught` (auto-logged on `mt path next → present_lesson`, and on
  every `mt lesson upsert` since authoring implies presenting)
- `quiz_authored` (on `mt quiz create`)
- `quiz_presented` (auto-logged on `mt path next → present_quiz`)
- `quiz_answered` (on `mt quiz answer`; payload carries rating and user_answer)
- `quiz_amended` (on `mt quiz update`)
- `quiz_removed` (on `mt quiz delete`)

The log is the source of truth for all learning activity: what the user was
shown and how they performed. Derived state is reconstructed from it. FSRS
card state, for instance, is reconstructed by replaying `quiz_answered`.

## Spaced repetition (FSRS)

- **One FSRS card per quiz question.**
- **No persistent card state.** The card state for any quiz is
  reconstructed by replaying its `quiz_answered` events through the
  FSRS algorithm in order. This is intentionally a pure function to
  keep the log as the sole source of truth
- **`mt path next` algorithm:**
  1. Earliest-due quiz card whose atom is in the merged view → `present_quiz`.
  2. Else, walk targets (and their prereqs) in topo order. For each
     incomplete atom, return the first applicable action:
     a. no lesson in the merged view → `create_lesson`
     b. lesson exists but the path hasn't taught it yet → `present_lesson`
     c. missing easy/medium/hard slot → `create_quiz`
     d. quiz never answered with `good`/`easy` → `present_quiz`
  3. Else → `done`.

  An atom is "complete" only once its lesson is in the merged view and
  all three difficulty quizzes have at least one correct answer in the
  event log. The walker doesn't move past an incomplete atom, so a
  freshly-taught atom always gets all three quizzes authored and
  answered before the next target's lesson is requested.

  "Correct" is any answer with a rating `Hard`, `Good`, or `Easy`.
  `Hard` is "got it right, with effort"; FSRS handles its shorter follow-up interval.
  The per-atom walker presents again immediately on rating `Again`.

## Teaching strategy: bottom-up and top-down

Every path has a strategy that controls how `mt path next` walks toward
the target atoms. It is a traversal mode, not part of the path's intent:
`mt path new --strategy` sets the initial value (default bottom-up), and
`mt path strategy <bottom-up|top-down>` switches it at any point. Atom
completeness is computed identically in both modes, so a learner can dive
at the target top-down, hit a wall, and flip to bottom-up to build foundations,
or the reverse. A bottom-up strategy ignores any subpath; switching back
to top-down resumes it.

**Bottom-up** (default) teaches foundations first: the post-order DFS
over the prerequisite DAG, which never reaches an atom until all prerequisites
are complete. Good when the learner is starting cold and wants the full mastery ladder.

**Top-down** teaches the target first and descends only when the learner
needs it. `mt path next` presents the next incomplete target directly,
without walking its prerequisites. A target is completed by its own lesson
and quizzes; prerequisites are not required or presented by default.
A top-down path is considered done when all target nodes are complete.

### Subpaths

If a top-down learner finds themselves stuck on a target, the agent may
compose a subpath: an ordered sequence of atoms ending in the target.
The sequence is chosen to unblock the learner and return back to the goal
(all missing prerequisites that are most relevant to the target).
A subpath is the only way prerequisites are learned a top-down path.

- `mt path subpath set --atoms a,b,…,<target>` replaces a path's subpath.
  The agent recomputes the subpath after discussion with the learner;
  the last atom must be the target the subpath drives toward.
- `mt path subpath clear` removes the subpath.

If a subpath is set, `mt path next` serves the first incomplete atom in
subpath order through the usual per-atom sequence (`create_lesson` →
`present_lesson` → `create_quiz` → `present_quiz`). Completed atoms are
skipped, never mutated. Completion is derived from the log exactly as
elsewhere, so `next` never edits the subpath. The route drains into the
target on its own; once the target (the tail) completes, the subpath is
cleared and `next` returns to the remaining targets.

Between edits the subpath is a deterministic on-rails iterator just like
the bottom-up DFS, but the agent and learner can rewrite or clear it on
any turn.

### `mt path next` priority (top-down)

1. Earliest-due quiz card whose atom is in the merged view → `present_quiz`
   (skipped under `--new`, as in bottom-up).
2. If a subpath is set, the first incomplete atom in subpath order.
3. Else the next incomplete target, in target order.
4. Else `done`.

## Tool I/O

- Structured output: AYML on stdout (one record per call).
- Progress / human-readable logs: stderr.
- Exit codes: `0` ok; `1` scheduler / state-read failure; `2` config /
  IO / validation.
- Multi-paragraph content (lesson bodies, quiz questions/answers/
  rubrics) is passed as text. Agents may pass them via heredoc
  to preserve newlines.

## Errors

A single crate-wide `Error` enum covers every failure mode. Every
fallible function returns `crate::Result<T>`. There is no per-module
error hierarchy.

## Out of scope for V1

- **Lesson remove.** Quiz tombstoning exists; lesson tombstoning would
  need an `overlay_removed_lessons` table and a corresponding event
  kind. Not yet motivated by a use case.
- **`mt relearn` and `mt hint`.** Designed but not wired.
- **`idle` action.** `mt next` only ever returns `done` or a real
  action today; `idle` (returns when nothing is due but future
  reviews exist) is reserved for when FSRS scheduling produces gaps
  worth signaling.
- **Goal → atom-set mapping.** `mt path new <goal>` relies on the agent
  to translate the free-text goal into a target set of atom IDs;
  `mt` stores the resulting list and orders it by prerequisite
  topology.
- **Adding new atoms or editing prerequisites via the CLI.** Curriculum
  topology changes are still hand-edited and `mt graph check`'d.
- **Multi-modal content** (diagrams, images, code execution).
- **Multi-user hosting.** Storage is single-user single-tenant; multi-
  user requires identity, per-user storage scoping, and an MCP/HTTP
  server layer. A personal hosted setup (one-user Fly VM with MCP) is
  reachable from this codebase with a modest server wrapper.
