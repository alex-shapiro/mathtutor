# Math Tutor

Math Tutor is a tool for learning math via a DAG of small lessons and quizzes.
It incorporates spaced repetition so that learned concepts stay learned.
It runs as a CLI and stores all data in [AYML](https://crates.io/crates/ayml)
text files. AYML is a safe, serde-compatible variant of YAML; the only
practical difference is that AYML uses triple-quoted multiline strings (like
Swift) instead of `|`, and disallows YAML's long tail of fringe features.

## Roles

`mt` is invoked as a tool from inside an existing LLM agent loop
(e.g. Claude Code, Codex). The two-actor split:

- **`mt`** owns scheduling, persistence, deterministic reuse, and graph
  validation.
- **The LLM agent** owns authoring lessons / quizzes, user-facing presentation,
  and grading of free-text answers.

`mt next` decides _what_ should be presented; the agent decides _how_ it is
presented. When a lesson or quiz is presented for the first time, the agent
authors it and writes it back into the graph; every subsequent presentation
is deterministic. If the user finds wording confusing, the agent calls
`mt amend …` and the canonical content is rewritten.

Each atom is one concept. Lessons are short (1–2 paragraphs, ≤ 2 minutes
reading, ≤ 1 theorem / rule / definition). Quizzes are short, free-text by
default, and depend only on the current lesson plus previously-taught
lessons — never on lookahead material.

## Commands

```bash
# Path lifecycle
mt new <GOAL> --atom <ID>...   # start a new learning path
mt state [--path P]            # current status of a path
mt next  [--path P]            # next action (AYML on stdout)

# LLM stores authored content
mt store lesson <ATOM>      --body TEXT
mt store quiz   <ATOM>      --difficulty D \
                            --question TEXT --answer TEXT [--rubric TEXT] \
                            [--type {free_text,multiple_choice}]

# LLM logs outcomes
mt answer  <QUIZ_ID> --rating {again,hard,good,easy}   # FSRS grade
mt skip    <ATOM> [--reason STR]
mt hint    <ATOM>
mt relearn <ATOM>

# LLM amends canonical content
mt amend lesson <ATOM> --body TEXT
mt amend quiz   <QUIZ_ID> [--question TEXT] [--answer TEXT] [--rubric TEXT]

# Maintenance
mt graph check                                   # validate curriculum graph
```

### `--atom` ID resolution on `mt new`

Each `--atom` argument may be:

- an **atom ID** (leaf concept, e.g. `tx.1.1`) — included as-is
- a **cluster ID** (any non-leaf node, e.g. `tx.1` or `tx.5`) — expanded
  to all atomic descendants
- a bare **area prefix** (e.g. `tx`) — expanded to every atom in that
  area

Mixing forms is allowed; results are deduplicated and topologically
sorted by prerequisite order before being stored as the path's
`target_atoms`.

All commands write a structured AYML record to the per-path event log;
agents read the log if they need history.

## Lifecycle of an atom

1. **Bare.** Atom exists in the canonical graph with `id`, `name`,
   `description`, `prerequisites`. No `lesson`, no `quizzes`.
2. **Lesson stored.** `mt next` returns `create_lesson`. The agent authors
   a body and calls `mt store lesson`. The body is persisted into the
   atom's `lesson:` field.
3. **Quiz stored (per slot, lazy).** `mt next` later returns
   `create_quiz` for some difficulty slot. The agent authors a question,
   reference answer, and optional rubric, then calls `mt store quiz`.
   The quiz is persisted into the atom's `quizzes:` list under a stable ID.
4. **Quiz answered.** `mt next` returns `present_quiz` with the stored
   `q`, `a`, `rubric`. The agent presents the question, grades the user's
   reply against the rubric, and calls `mt answer <quiz-id> --rating …`.

Each lesson and each individual quiz is generated lazily — only when the
scheduler first asks for it. An atom can be in a partial state (lesson
exists, only the easy quiz exists) for arbitrarily long.

## `mt next` I/O

`mt next` writes one AYML record to stdout. Top-level shape:

```yaml
schema_version: 1
action: create_lesson | create_quiz | present_lesson | present_quiz | done | idle
path: <path-id>
payload: ... action-specific (see below)
```

Conventions for every payload:

- IDs are stable curriculum-graph identifiers (`la.5.4.7`, `la.5.4.7.q2`).
- Multi-paragraph fields (`lesson`, `question`, `answer`, `rubric`) are
  AYML triple-quoted strings.
- `next_step` is an informational hint for the agent; it is the canonical
  command the agent should call after acting on this output. The agent
  may derive the same command from the spec — the field exists for
  ergonomics, not authority.
- All atoms `mt next` returns satisfy the prerequisite invariant: every
  direct prerequisite has a stored lesson. The agent does not have to
  worry about teaching out of order.

### `create_lesson`

The next path-target atom has no stored lesson. The agent should author
one (1–2 paragraphs, ≤ 1 theorem / rule / definition), present it to the
user, and call `mt store lesson` to persist.

```yaml
action: create_lesson
path: p_2026_05_09_173_42
payload:
  atom:
    id: la.5.4.7
    name: "Singular values from A*A eigenvalues"
    description: "σᵢ² are the non-zero eigenvalues of A* A."
  prerequisites:                          # direct prereqs only
    - id: la.5.4.2
      name: "Singular value"
      description: "Diagonal entry of Σ; non-negative real number."
      lesson: """..."""                   # included; every direct prereq has a lesson
    - id: la.4.1.1
      name: "Eigenvalue / eigenvector"
      description: "Av = λv with v ≠ 0; λ ∈ 𝔽 is the eigenvalue."
      lesson: """..."""
  next_step: "mt store lesson la.5.4.7 --body TEXT"
```

### `create_quiz`

The atom has a lesson but a difficulty slot has no quiz yet. The agent
authors a (preferably free-text) question + reference answer + optional
rubric, presents the question to the user, and persists via
`mt store quiz`. The reply rating is logged with `mt answer` afterwards.

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
  existing_quizzes:                       # other quizzes already on this atom (avoid duplicates)
    - id: la.5.4.7.q1
      difficulty: easy
      question: """..."""
  prerequisites:
    - id: la.5.4.2
      name: "Singular value"
      description: "..."
      lesson: """..."""
  next_step: "mt store quiz la.5.4.7 --difficulty medium --question TEXT --answer TEXT [--rubric TEXT]"
```

### `present_lesson`

A previously-taught lesson is being re-presented (e.g. after `mt relearn`).
The agent shows the stored body verbatim — not re-authored. After
showing, the agent calls `mt next` again.

```yaml
action: present_lesson
path: p_2026_05_09_173_42
payload:
  atom:
    id: la.5.4.7
    name: "Singular values from A*A eigenvalues"
    description: "σᵢ² are the non-zero eigenvalues of A* A."
    lesson: """...stored body..."""
  reason: relearn_requested
  history:
    repetitions: 3                        # past `lesson_presented` events for this atom
    last_presented_at: 2026-05-08T14:23:11Z
    time_since_last: PT24H17M             # ISO 8601 duration; convenience for the agent
  next_step: "mt next"
```

### `present_quiz`

A quiz card is due. The agent shows the stored question to the user,
grades the user's free-text reply against the reference answer + rubric,
and reports the FSRS grade with `mt answer`.

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
    time_since_last: PT24H17M             # ISO 8601 duration
    correct_count: 3                      # ratings in {good, easy}
    total_count: 5
    correct_pct: 60                       # integer percentage, convenience
    recent_ratings: [easy, good, hard, again, good]   # most recent first
  next_step: "mt answer la.5.4.7.q2 --rating {again|hard|good|easy}"
```

### `done` / `idle`

Two terminal states with different agent UX:

- `done` — every target atom of the path has been taught and has at
  least one quiz answered with `good` or `easy`. The path goal is
  considered reached (modulo ongoing retention reviews).
- `idle` — nothing is due right now and there are no pending content slots
  to author, but future reviews exist. The agent should suggest the user
  return later.

```yaml
action: done # or: idle
path: p_2026_05_09_173_42
payload:
  next_due_at: 2026-05-12T09:00:00Z # null if no future cards
  message: "Path complete." # or: "Nothing due right now."
```

### Errors

Failures are written to stderr; stdout stays a single valid AYML record
or is empty. Exit codes follow the global convention (`1` for scheduler
failure, `2` for config / IO).

## Storage

V1 uses a single tier — the canonical curriculum graph itself.

```
curriculum/graph/
  manifest.ayml
  areas/<NN>-<slug>.ayml      # canonical concept tree; lessons + quizzes added here

~/.mathtutor/                 # exact location TBD; per-user
  paths/<path-id>/
    path.ayml                 # goal, target atoms, FSRS card state
    log.ayml                  # append-only event log
```

V1 limitation: `mt amend …` writes directly into the canonical graph.
Single-user only until V2 introduces a per-user overlay.

## Lesson & quiz persistence schema

Authored content extends each atom in place:

```yaml
- id: la.5.4.7
  name: "Singular values from A*A eigenvalues"
  description: "σᵢ² are the non-zero eigenvalues of A* A."
  prerequisites: [la.5.4.2, la.4.1.1]

  # filled in by `mt store lesson`
  lesson: """
    ...1–2 paragraph Markdown body...
    """

  # one entry per question, filled in by `mt store quiz`.
  # IDs are stable: <atom-id>.q<n>, never reused.
  quizzes:
    - id: la.5.4.7.q1
      difficulty: easy            # easy | medium | hard
      type: free_text             # free_text (default) | multiple_choice
      question: """..."""
      answer:   """..."""         # reference answer
      rubric:   """..."""         # optional grading guidance
```

Free-text questions are strongly preferred. Multiple-choice is allowed
only as a deliberate exception (e.g. distinguishing a definition from a
common confusable). Question wording must depend only on knowledge from
the current lesson and previously-taught lessons; no lookahead.

## Event log

One append-only AYML file per learning path. Every event has a top-level
`ts` field in RFC 3339 / ISO 8601 format with UTC timezone — required,
not optional — so the log can be re-played and `time_since_last` /
`repetitions` derived without additional state. Each entry:

```yaml
- ts: 2026-05-09T18:42:01Z # required, RFC 3339 UTC
  type: quiz_answered
  path: <path-id>
  atom: la.5.4.7
  quiz: la.5.4.7.q1
  payload:
    rating: good
```

Event types (initial set; expect to grow):

`path_created`, `path_updated`,
`lesson_authored`, `lesson_taught`, `lesson_amended`, `lesson_relearn_requested`,
`quiz_authored`, `quiz_presented`, `quiz_answered`, `quiz_skipped`, `quiz_amended`,
`hint_requested`, `atom_amended`.

The log is the source of truth for what the user has seen and how they
performed; FSRS state in `path.ayml` is a derived index that can be
recomputed from the log.

## Spaced repetition (FSRS)

- **One FSRS card per quiz question.** Not per atom, not per difficulty
  slot — per individual question. Memorizing an easy question shouldn't
  pull a hard one out of rotation.
- Card state stored in `path.ayml`, keyed by quiz ID.
- `mt next` algorithm (sketch — exact priority is tunable):
  1. Earliest-due quiz card whose atom has a stored lesson → `present_quiz`.
  2. Else, the next path-target atom that lacks a lesson → `create_lesson`.
  3. Else, an already-taught atom with an unfilled difficulty slot →
     `create_quiz`.
  4. Else, advance the path or return `done`.

The `fsrs` crate is the current default; not 100% confirmed — needs to be
proved out during the PoC. If FSRS doesn't fit cleanly we'll either swap
implementations or implement the scheduler ourselves.

## Tool I/O

- Structured output: AYML on stdout.
- Progress / human-readable logs: stderr.
- Exit codes: `0` ok; `1` validation or scheduling failure; `2` config error.
- Multi-paragraph content (lesson bodies, quiz questions/answers/rubrics)
  is passed as `TEXT` arguments — literal strings on the command line.

## Crates

- `argh` — CLI parsing
- `ayml` + `serde` — data serialization
- `fsrs` — spaced-repetition scheduling (tentative; see above)
- `tracing` — debug logging
- `thiserror` — error types

## Out of scope for V1

- **Per-user overlay** of authored content. V1 amends the canonical graph
  in place; multi-user sharing comes later.
- **Goal → atom-set mapping.** `mt new <goal>` relies on the agent to
  translate the free-text goal into a target set of atom IDs; `mt`
  stores the resulting list and orders it by prerequisite topology.
- **Adding new atoms or editing prerequisites** via the CLI. Curriculum
  topology changes are still hand-edited + `mt graph check`'d.
- **Multi-modal content** (diagrams, images, code execution).
