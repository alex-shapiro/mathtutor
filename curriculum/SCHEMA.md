# AYML Schema

The curriculum graph is stored in [AYML](https://crates.io/crates/ayml) files under `graph/` and validated with `mt graph check`.

## Versioning

- **`schema_version: 2`** (current): Uses recursive `children:` lists at arbitrary depth.
- **`schema_version: 1`** (legacy): Uses fixed-depth `topics:` and `leaves:` keys.

## Files

- `graph/manifest.ayml`: Top-level area registry.
- `graph/areas/<NN>-<slug>.ayml`: Area definition files.

## `manifest.ayml`

```yaml
schema_version: 1
areas:
  - prefix: fnd
    slug: foundations
    file: areas/00-foundations.ayml
    summary: "Logic, sets, proof, basic combinatorics, computation models."
```

`slug` and `prefix` must match the `area` and `prefix` fields in the referenced area file.

## Area file (`schema_version: 2`)

```yaml
schema_version: 2
area: foundations
prefix: fnd
summary: "Area summary."
motivation: """
  Multi-line motivation.
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
```

## Atomicity

A node is an **atom** if `children` is omitted or empty. A node with non-empty `children` is a **cluster**.

## Field reference

### Area fields

| Field              | Required | Description                             |
| ------------------ | -------- | --------------------------------------- |
| `schema_version`   | Yes      | `2` (or `1` for legacy files)           |
| `area`             | Yes      | Area slug matching the manifest         |
| `prefix`           | Yes      | ID prefix matching the manifest         |
| `summary`          | Yes      | Short area description                  |
| `motivation`       | Yes      | Multi-line area motivation              |
| `cross_references` | No       | Related area slugs                      |
| `children`         | Yes (v2) | Top-level nodes in the area             |
| `topics`           | Yes (v1) | Legacy three-level hierarchy (v1 only)  |

### Node fields

| Field           | Required    | Description                                                    |
| --------------- | ----------- | -------------------------------------------------------------- |
| `id`            | Yes         | Dot-separated identifier (for example, `fnd.1.1.5`)            |
| `name`          | Yes         | Node title                                                     |
| `description`   | Recommended | Scope summary (for clusters) or concept summary (for atoms)    |
| `prerequisites` | No          | Direct prerequisite node IDs (atoms or clusters)               |
| `children`      | No          | Child nodes; omit or set to `[]` for an atom                   |
| `terminal`      | No          | Set to `true` on leaf atoms that do not have dependents        |
| `lesson`        | No          | Inline Markdown lesson body (1–2 paragraphs)                   |
| `quizzes`       | No          | Inline quiz definitions                                        |

Inline quiz format:

```yaml
quizzes:
  - id: fnd.1.1.1.q1
    difficulty: easy        # easy | medium | hard
    type: free_text         # free_text (default) | multiple_choice
    question: "..."
    answer: "..."
    rubric: "..."           # optional
```

## `terminal`

`mt graph check` reports an error if an atom is not listed as a prerequisite by any other node. Set `terminal: true` on an intentional leaf endpoint to suppress this error. If a cluster is referenced as a prerequisite, all of its descendant atoms are considered referenced.

## ID rules

- Every ID starts with the area `prefix`, followed by dot-separated positive integers without leading zeros (for example, `la.5.4.7`).
- Each child ID appends a single segment to its parent ID.
- IDs are globally unique, permanent, and never reused after deletion.
