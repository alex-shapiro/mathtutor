# YAML schema

The curriculum graph lives entirely in YAML under `graph/`. This document
is human-readable; the YAML files are authoritative.

## Versioning

- **`schema_version: 2`** — current. Recursive `children:` at any depth.
  Each atomic node is one mini-lesson: 1–2 paragraphs, ≤ 2 minutes
  reading, ≤ 1 theorem / rule / definition.
- **`schema_version: 1`** — legacy. Fixed depth (area → topic → leaf)
  with `topics:` / `leaves:` keys. Areas not yet migrated to v2 still
  use v1; loaders should dispatch on `schema_version`.

## Files

- `graph/manifest.yaml` — top-level area registry.
- `graph/areas/<NN>-<slug>.yaml` — one file per top-level area.

## `manifest.yaml`

```yaml
schema_version: 1
areas:
  - prefix: fnd
    slug: foundations
    file: areas/00-foundations.yaml
    summary: "Logic, sets, proof, basic combinatorics, computation models."
  # ...
```

## Per-area file (v2)

```yaml
schema_version: 2
area: foundations
prefix: fnd
summary: "One-line area summary."
why_for_dl: |
  Multi-line motivation, Markdown allowed.
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
- Optional small quiz (2–4 short Q/A pairs) for spaced repetition.

A node *with* `children` is a **cluster** — purely organizational. A
cluster may still have a `description` (one-line summary of what its
descendants cover) and `prerequisites` (coarse ordering hints), but no
`lesson` body. Clusters can be nested arbitrarily deep.

## Field reference

### Area-level

| field | required | description |
|---|---|---|
| `schema_version` | yes | `2` |
| `area` | yes | slug; matches manifest |
| `prefix` | yes | short ID prefix |
| `summary` | yes | one-line area description |
| `why_for_dl` | yes | multi-line motivation |
| `cross_references` | no | list of adjacent area slugs |
| `children` | yes | top-level concept tree |

### Node-level (any depth)

| field | required | description |
|---|---|---|
| `id` | yes | dotted path, e.g. `fnd.1.1.5` |
| `name` | yes | human-readable name |
| `description` | recommended | one-line scope (cluster) or hook (atom) |
| `children` | no | sub-nodes; omit or `[]` for atom |
| `prerequisites` | no | direct prereq IDs (any depth) |
| `relevant_for` | no | downstream area slugs |
| `tags` | no | free-form tags |
| `status` | no | `stub` (default) / `drafted` / `reviewed` |

### Atom-only fields (filled in pass 2)

| field | required | description |
|---|---|---|
| `lesson` | no | inline Markdown body, 1–2 paragraphs |
| `lesson_path` | no | external Markdown file with the body |
| `quiz` | no | list of `{q, a}` pairs for spaced repetition |
| `estimated_minutes` | no | typically 1–2 |
| `references` | no | citation strings |

## ID rules

- IDs are **stable**. Renaming or relocating a concept never changes
  its ID; the slug, file path, and `name` may change but the ID may not.
- Deleted IDs are **never reused**.
- Nesting depth is unlimited; atomicity is a content choice, not a depth
  choice. A 4-level path like `la.5.4.7` is normal.
- v1 leaf IDs (e.g. `la.5.4` for SVD) remain valid in v2 — they become
  cluster nodes whose `children` list contains the atomic decomposition.
- Cross-area prerequisites use the full ID (e.g. `prob.8.4` from inside
  a `dif.x.y.z` atom).

## Conventions

- One canonical home per atom; cross-link via `prerequisites` /
  `relevant_for` rather than duplicating leaves.
- `prerequisites:` lists **direct** dependencies. The loader computes
  transitive closure.
- An atom's `prerequisites` may point to other atoms or to cluster IDs;
  pointing at a cluster means "you need that whole cluster."
- Pass 1 fills `id`, `name`, `description`, `prerequisites`, and the
  depth structure. Pass 2 fills `lesson`, `quiz`, `estimated_minutes`,
  `references`.
- One rule / theorem / definition per atom. If you find yourself
  describing two unrelated rules in one atom, split it.
