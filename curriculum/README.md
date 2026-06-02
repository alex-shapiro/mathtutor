# Math Tutor Curriculum Graph

A hierarchical, prerequisite-aware concept graph covering the mathematical
foundations needed to deeply understand contemporary deep-learning research,
with a focus on:

- Large transformer language models
- Diffusion / flow-matching generative models
- Joint-embedding predictive architectures (JEPA) and self-supervised learning

## Layout

```
curriculum/
├── README.md            (this file)
├── SCHEMA.md            (YAML schema spec)
└── graph/
    ├── manifest.yaml    (top-level area registry)
    └── areas/
        ├── 00-foundations.yaml
        ├── 01-linear-algebra.yaml
        ├── ...
        └── 15-reinforcement-learning.yaml
```

The graph is machine-readable: every node lives in YAML and the whole
thing loads with any standard YAML library (e.g. `serde_yaml` for the
existing Rust crate at `src/main.rs`).

## Two-pass build

- **Pass 1 — skeleton (current):** every area, topic, and leaf is
  enumerated in `graph/areas/*.yaml` with stable IDs, one-line
  descriptions, and direct prerequisites. No prose bodies yet.
  The shape of the graph is auditable; reorganize / add / cut here
  before fleshing out.
- **Pass 2 — leaves:** each leaf gets a body — either inline in the YAML
  under a `body:` key, or as a sibling Markdown file referenced by a
  `body_path:` key. See `SCHEMA.md`.

## ID conventions

- Each area has a short prefix: `fnd`, `la`, `ana`, `prob`, `opt`, `dis`,
  `num`, `geom`, `dyn`, `inf`, `lt`, `nn`, `tx`, `dif`, `ssl`, `rl`.
- Topic ID: `<prefix>.<topic-num>` (e.g. `la.5` = matrix factorizations).
- Leaf ID: `<prefix>.<topic-num>.<leaf-num>` (e.g. `la.5.4` = SVD).
- IDs are **stable** — if a concept is renamed or relocated, keep its ID.
- New leaves get fresh trailing numbers; deleted IDs are not reused.

## Hierarchy vs prerequisites

Two distinct DAGs share the same nodes:

- **Taxonomic hierarchy** — leaves belong to topics, topics belong to
  areas. Implicit in the YAML's nesting. Used for navigation, grouping,
  file layout.
- **Prerequisite graph** — "you should understand X before Y." Encoded
  via the `prerequisites:` list on each leaf. Crosses topic and area
  boundaries freely. Used for ordering and reading-path generation.

A loader (e.g. `src/main.rs`) can parse this into a queryable graph:
topo-sort, "what do I need before X", "shortest path to understand
paper Y", coverage-vs-target analysis, etc.

## Schema versions

The graph is migrating from v1 (fixed 3-level: area → topic → leaf) to
v2 (recursive `children:` at any depth, with atomic mini-lesson nodes).

- **v2 (atomized):** `00-foundations.yaml`, `01-linear-algebra.yaml`.
  Atomic nodes follow "one rule / theorem / definition per lesson,
  ≤ 2 minutes reading."
- **v1 (legacy):** `02-analysis.yaml` … `15-reinforcement-learning.yaml`.
  Will be migrated incrementally once the `fnd` / `la` granularity is
  validated.

A loader should dispatch on the file's `schema_version` field. See
`SCHEMA.md` for both formats.

## Status

Pass 1 skeleton is complete in v1 form across all 16 areas. v2
atomization is underway (`fnd` + `la` first); audit those for
granularity, naming, and prerequisite density before the other 14 areas
are migrated.
