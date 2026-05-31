# Math Tutor

Math Tutor is a tool for learning math via a DAG of small lessons and
quizzes. It incorporates spaced repetition so that learned concepts stay
learned. It runs as a CLI: the curriculum graph is compiled into the
binary, per-user state lives as [AYML](https://crates.io/crates/ayml)
text files under `~/.mathtutor/`. AYML is a safe, serde-compatible variant
of YAML; the only practical difference is that AYML uses triple-quoted
multiline strings (like Swift) instead of `|`, and disallows YAML's long
tail of fringe features.

## Roles

`mt` is invoked as a tool from inside an existing LLM agent (e.g. Claude, ChatGPT, Gemini):

- `mt` owns scheduling, persistence, deterministic reuse, graph
  validation, and the per-path overlay where the user's authored
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
mt path new <GOAL> --atom <ID>...  # start a new learning path
mt path state [--path P]           # one-screen status summary
mt path next  [--path P]           # next scheduled action (AYML on stdout)
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
base shape — fields are added via `skip_serializing_if`.

Every command that writes appends a structured event to the per-path
log (see "Event log" below). Agents read the log if they need history;
nothing else is needed to reconstruct user state.

### `--atom` ID resolution on `mt path new`

Each `--atom` argument may be:

- an **atom ID** (leaf concept, e.g. `tx.1.1`) — included as-is
- a **cluster ID** (any non-leaf node, e.g. `tx.1` or `tx.5`) — expanded
  to all atomic descendants
- a bare **area prefix** (e.g. `tx`) — expanded to every atom in that
  area

Mixing forms is allowed; results are deduplicated and topologically
sorted by prerequisite order before being stored as the path's
`target_atoms`.

## Lifecycle of an atom (within a path)

1. **Bare in the path.** Atom exists in the shipped curriculum with
   `id`, `name`, `description`, `prerequisites`. The path's overlay
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
   — which appends to the overlay.
5. **Quiz answered.** `mt path next` returns `present_quiz`. The agent
   presents the stored question, grades the user's reply against the
   reference answer + rubric, and calls `mt quiz answer <quiz-id>
--rating …`.

Each lesson and each individual quiz is generated lazily — only when the
scheduler first asks for it. An atom can be in a partial state (lesson
exists, only the easy quiz exists) for arbitrarily long.

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
- `next_step` is an informational hint for the agent — the canonical
  command to call after acting on this output.
- All atoms `mt path next` returns satisfy the prerequisite invariant:
  every direct prerequisite already has a stored lesson in the merged
  view.

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
any quiz. The agent shows the body verbatim — not re-authored — then
calls `mt path next` again. `mt path next` auto-logs `lesson_taught`
when it returns this action, so no explicit "I taught it" command is
needed.

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

## Storage

The curriculum graph is compiled into the binary at build time via
`include_dir!`. A shipped `mt` runs from any cwd; no checked-out repo
required. For development against a working tree, `--graph DIR` or the
`MT_GRAPH` env var override the embedded copy.

```
curriculum/graph/                # source for the embedded copy
  manifest.ayml
  areas/<NN>-<slug>.ayml

~/.mathtutor/                    # per-user state (overridable via $MATHTUTOR_HOME)
  paths/<path-id>/
    path.ayml                    # immutable intent: id, goal, created_at, target_atoms
    log.ayml                     # append-only event stream
    overlay.ayml                 # authored content (lessons, quizzes, amendments, removals)
```

Three roles, three files, all distinct:

| File           | Role                      | Mutability                                                       |
| -------------- | ------------------------- | ---------------------------------------------------------------- |
| (embedded)     | shipped curriculum        | recompile only                                                   |
| `path.ayml`    | per-path intent           | written once at `mt path new`; never updated                     |
| `log.ayml`     | per-path history          | append-only                                                      |
| `overlay.ayml` | per-path authored content | mutated by `mt lesson upsert` / `mt quiz {create,update,delete}` |

## Per-path overlay

User-authored lessons and quizzes live in the path's overlay, not the
shipped curriculum:

```yaml
schema_version: 1
atoms:
  la.5.4.7:
    lesson: """
      ...authored lesson body, if shipped graph had none...
      """
    quizzes:                          # added or amended
      - id: la.5.4.7.q4
        difficulty: medium
        question: """..."""
        answer: """..."""
    removed:                          # tombstoned quiz ids; merge skips these
      - la.5.4.7.q2
```

Merge semantics (`Graph::load_for_path`): an overlay lesson, quiz, or
tombstone always overrides a shipped item with the same ID. Tombstones
override everything. Concretely:

- **Lesson** — if the overlay has one, use it; otherwise use the
  shipped lesson if present. `mt lesson upsert` is an upsert; a second
  call replaces the body.
- **Quizzes** — start with shipped, replace any with the same id from
  the overlay (this is how `mt quiz update` works), then append overlay
  quizzes whose ids don't match shipped (added by `mt quiz create`),
  then filter out anything whose id appears in `overlay_removed_quizzes`.
- **Prerequisites / cluster structure** — not overridable. The shape
  of the curriculum is fixed by the shipped graph; the overlay only
  carries content.

Blast-radius is per-path on purpose: an unaudited lesson authored under
path A doesn't leak into path B that targets the same atom. `mt graph
dump` prints the user overlay (shared across paths) for review and
eventual merge back into the canonical curriculum.

## Event log

One append-only AYML file per learning path. Every event has a top-level
`ts` field in RFC 3339 / ISO 8601 format with UTC timezone — required,
not optional — so the log can be re-played and `time_since_last` /
`repetitions` derived without additional state. Each entry:

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

Implemented event kinds:

- `path_created`
- `lesson_authored` (on `mt lesson upsert` when no prior lesson existed
  in the merged view)
- `lesson_amended` (on `mt lesson upsert` when a lesson — shipped or
  overlay — was already in the merged view)
- `lesson_taught` (auto-logged on `mt path next → present_lesson`, and on
  every `mt lesson upsert` since authoring implies presenting)
- `quiz_authored` (on `mt quiz create`)
- `quiz_presented` (auto-logged on `mt path next → present_quiz`)
- `quiz_answered` (on `mt quiz answer`; payload carries rating and user_answer)
- `quiz_amended` (on `mt quiz update`)
- `quiz_removed` (on `mt quiz delete`)

The log is the source of truth for what the user has seen and how they
performed. FSRS state is derived from `quiz_answered` events on demand
(`crate::cards`) — not persisted as a separate cache.

## Spaced repetition (FSRS)

- **One FSRS card per quiz question.** Not per atom, not per difficulty
  slot — per individual question. Memorizing an easy question shouldn't
  pull a hard one out of rotation.
- **No persistent card state.** The card state for any quiz is
  reconstructed by replaying its `quiz_answered` events through the
  FSRS algorithm in order. This is intentionally a pure function of the
  log — keeps the invariant "log is source of truth" sharp, and at our
  scale (≤ ~10k events per path) the replay cost stays under ~50ms.
  A persistent `cards.ayml` cache becomes interesting beyond that scale.
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

  "Correct" means anything except `Again` — i.e. `Hard`, `Good`, or
  `Easy`. `Hard` is "got it right, with effort"; FSRS handles its
  shorter follow-up interval. The per-atom walker only re-presents
  immediately on `Again`, so a `Hard`-rated card doesn't loop back the
  moment the user finishes answering it.

## Tool I/O

- Structured output: AYML on stdout (one record per call).
- Progress / human-readable logs: stderr.
- Exit codes: `0` ok; `1` scheduler / state-read failure; `2` config /
  IO / validation.
- Multi-paragraph content (lesson bodies, quiz questions/answers/
  rubrics) is passed as `TEXT` arguments — literal strings on the
  command line. Agents typically pass them via heredoc to preserve
  newlines.

## Errors

A single crate-wide `Error` enum covers every failure mode. Every
fallible function returns `crate::Result<T>`. There is no per-module
error hierarchy.

## Crates

- `argh` — CLI parsing
- `ayml` + `serde` — data serialization (per-path files only;
  curriculum is compiled-in bytes)
- `chrono` — timestamps
- `fsrs` — spaced-repetition scheduling
- `include_dir` — compile-time curriculum embedding
- `thiserror` — error type derive
- `tracing` — debug logging (unused for output; reserved for future
  observability)

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
  server layer — none of which is built. A personal hosted setup
  (one-user Fly VM with MCP) is reachable from this codebase with a
  modest server wrapper; multi-tenant SaaS is a separate project.
