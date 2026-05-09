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

`mt next` decides *what* should be presented; the agent decides *how* it is
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
mt new   <GOAL>                                  # start a new learning path
mt state [--path P]                              # current status of a path
mt next  [--path P]                              # next action (AYML on stdout)

# LLM authors content (only after `mt next` says it's needed)
mt teach         <ATOM>     --body FILE
mt quiz-authored <ATOM>     --difficulty D \
                            --question FILE --answer FILE [--rubric FILE] \
                            [--type {free_text,multiple_choice}]

# LLM logs outcomes
mt answer  <QUIZ_ID>        --rating {again,hard,good,easy}   # FSRS grade
mt skip    <ATOM>           [--reason STR]
mt hint    <ATOM>
mt relearn <ATOM>

# LLM amends canonical content
mt amend lesson <ATOM>      --body FILE
mt amend quiz   <QUIZ_ID>   [--question FILE] [--answer FILE] [--rubric FILE]

# Maintenance
mt graph check                                   # validate curriculum graph
```

All commands write a structured AYML record to the per-path event log;
agents read the log if they need history.

## Lifecycle of an atom

1. **Bare.** Atom exists in the canonical graph with `id`, `name`,
   `description`, `prerequisites`. No `lesson`, no `quizzes`.
2. **Lesson taught.** `mt next` returns `needs_lesson`. The agent authors
   a body and calls `mt teach`. The body is persisted into the atom's
   `lesson:` field.
3. **Quiz authored (per slot, lazy).** `mt next` later returns
   `needs_quiz` for some difficulty slot. The agent authors a question,
   reference answer, and optional rubric, then calls `mt quiz-authored`.
   The quiz is persisted into the atom's `quizzes:` list under a stable ID.
4. **Quiz answered.** `mt next` returns `present_quiz` with the stored
   `q`, `a`, `rubric`. The agent presents the question, grades the user's
   reply against the rubric, and calls `mt answer <quiz-id> --rating …`.

Each lesson and each individual quiz is generated lazily — only when the
scheduler first asks for it. An atom can be in a partial state (lesson
exists, only the easy quiz exists) for arbitrarily long.

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

  # filled in by `mt teach`
  lesson: """
    ...1–2 paragraph Markdown body...
    """

  # one entry per question, filled in by `mt quiz-authored`.
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

One append-only AYML file per learning path. Each entry:

```yaml
- ts: 2026-05-09T18:42:01Z
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
  pull a hard one out of due rotation.
- Card state stored in `path.ayml`, keyed by quiz ID.
- `mt next` algorithm (sketch — exact priority is tunable):
  1. Earliest-due quiz card whose atom has a stored lesson → `present_quiz`.
  2. Else, the next path-target atom that lacks a lesson → `needs_lesson`.
  3. Else, an already-taught atom with an unfilled difficulty slot →
     `needs_quiz`.
  4. Else, advance the path or return `done`.

The `fsrs` crate is the current default; not 100% confirmed — needs to be
proved out during the PoC. If FSRS doesn't fit cleanly we'll either swap
implementations or implement the scheduler ourselves.

## Tool I/O

- Structured output: AYML on stdout.
- Progress / human-readable logs: stderr.
- Exit codes: `0` ok; `1` validation or scheduling failure; `2` config error.
- Multi-paragraph content (lesson bodies, quiz questions/answers/rubrics)
  is passed via `FILE` arguments — paths to text files, or `-` for stdin —
  so the CLI stays clean for the agent to drive.

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
