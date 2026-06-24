# AYML schema

The curriculum graph lives entirely in [AYML](https://crates.io/crates/ayml)
under `graph/` — a safe serde-compatible variant of YAML with Swift-style
triple-quoted multiline strings (`"""…"""`) and none of YAML's fringe
features. This document is human-readable; the files, and the loader in
`src/graph.rs`, are authoritative. `mt graph check` validates the whole
graph.

## Versioning

- **`schema_version: 2`** — current; every shipped area uses it. Recursive
  `children:` at any depth. Each atomic node is one mini-lesson: 1–2
  paragraphs, ≤ 2 minutes reading, ≤ 1 theorem / rule / definition.
- **`schema_version: 1`** — legacy. Fixed depth (area → topic → leaf) with
  `topics:` / `leaves:` keys. The loader still dispatches on
  `schema_version`, but no shipped area uses v1 any more.

## Files

- `graph/manifest.ayml` — top-level area registry.
- `graph/areas/<NN>-<slug>.ayml` — one file per top-level area.

## `manifest.ayml`

```yaml
schema_version: 1
areas:
  - prefix: fnd
    slug: foundations
    file: areas/00-foundations.ayml
    summary: "Logic, sets, proof, basic combinatorics, computation models."
  # ...
```

The manifest's own `schema_version` is independent of the per-area schema
version. `slug` and `prefix` must match the `area` and `prefix` fields
inside the referenced file.

## Per-area file (v2)

```yaml
schema_version: 2
area: foundations
prefix: fnd
summary: "One-line area summary."
motivation: """
  Multi-line motivation, why this area matters.
  """
cross_references:
  - linear-algebra
  - analysis

children:
  - id: fnd.1
    name: "Logic and proof"
    description: "Propositions, predicates, quantifiers, proof techniques."
    children:
      - id: fnd.1.1
        name: "Propositional logic"
        description: "Boolean reasoning at sentence level."
        children:
          - id: fnd.1.1.1
            name: "Proposition"
            description: "Declarative statement with a truth value."
            prerequisites: []
            # no children → atomic
```

## Atomicity

A node is **atomic** iff it has no `children` (or `children: []`). Atomic
nodes are mini-lessons:

- 1–2 paragraphs of reading (~1–2 minutes).
- At most one theorem, rule, or definition.

A node *with* `children` is a **cluster** — purely organizational. A
cluster may still have a `description` (one-line summary of what its
descendants cover) and `prerequisites` (coarse ordering hints), but no
`lesson` body. Clusters nest arbitrarily deep.

## Field reference

### Area-level

| field | required | description |
|---|---|---|
| `schema_version` | yes | `2` |
| `area` | yes | slug; matches manifest |
| `prefix` | yes | short ID prefix; matches manifest |
| `summary` | yes | one-line area description |
| `motivation` | yes | multi-line motivation |
| `cross_references` | no | list of adjacent area slugs (documentation only) |
| `children` | yes (v2) | top-level concept tree |
| `topics` | yes (v1) | legacy three-level tree; v1 only |

### Node-level (any depth)

| field | required | description |
|---|---|---|
| `id` | yes | dotted path, e.g. `fnd.1.1.5` |
| `name` | yes | human-readable name |
| `description` | recommended | one-line scope (cluster) or hook (atom) |
| `prerequisites` | no | direct prereq IDs; may target atoms or clusters |
| `children` | no | sub-nodes; omit or `[]` for an atom |
| `terminal` | no | `true` marks a leaf that intentionally has no dependents (see below) |

The loader also accepts `relevant_for`, `tags`, `status`, and a node-level
`difficulty` for forward compatibility, but no shipped area uses them and
nothing reads them; prefer `prerequisites` and `cross_references`.

### Atom content (optional; usually authored at runtime)

Most atoms ship as metadata only. Lesson and quiz bodies are normally
authored lazily by the agent on first presentation and stored in the
per-user overlay (see `docs/design.md`). An atom may instead ship them
inline when worth fixing canonically; the overlay overrides inline content.

| field | description |
|---|---|
| `lesson` | triple-quoted Markdown body, 1–2 paragraphs |
| `quizzes` | list of quiz cards (shape below) |

A quiz card:

```yaml
quizzes:
  - id: fnd.1.1.1.q1        # <atom-id>.q<n>
    difficulty: easy        # easy | medium | hard
    type: free_text         # free_text (default) | multiple_choice
    question: "…"
    answer: "…"             # reference answer for grading
    rubric: "…"             # optional grading guidance
```

## `terminal`

The orphan check in `mt graph check` flags any atom that no other node
lists as a prerequisite — usually a sign of a forgotten edge. A genuine
leaf with no dependents (a capstone application atom, a dead-end fact) sets
`terminal: true` to opt out. A prerequisite reference to a *cluster* covers
all its descendant atoms, so those need no `terminal` flag.

## ID rules

Enforced by `mt graph check`:

- Every ID starts with its area `prefix`; each further segment is a
  positive integer with no leading zeros (`la.5.4.7`).
- A node's ID extends its parent's by exactly one segment.
- IDs are **globally unique** and **stable**: renaming or relocating a
  concept never changes its ID; the slug, file path, and `name` may change.
- Deleted IDs are **never reused**.
- Nesting depth is unlimited; atomicity is a content choice, not a depth
  choice. A 4-level path like `la.5.4.7` is normal.
- Cross-area prerequisites use the full ID (e.g. `prob.8.4` from inside a
  `dif.x.y.z` atom). The prerequisite graph is acyclic and crosses area
  boundaries freely.

## Conventions

- One canonical home per atom; cross-link via `prerequisites` rather than
  duplicating a concept.
- `prerequisites:` lists **direct** dependencies only; the loader computes
  the transitive closure.
- One rule / theorem / definition per atom. If you are describing two
  unrelated rules in one atom, split it.
