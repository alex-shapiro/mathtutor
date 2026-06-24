# Math Tutor Curriculum Graph

A hierarchical, prerequisite-aware concept graph. Its center of gravity is
the mathematics needed to deeply understand contemporary deep-learning
research — large transformer language models, diffusion / flow-matching
generative models, and self-supervised / JEPA architectures — but it also
covers adjacent computer science where that understanding bottoms out in
systems (e.g. GPU programming).

## Layout

```
curriculum/
├── README.md            (this file)
├── SCHEMA.md            (authoritative schema spec)
└── graph/
    ├── manifest.ayml    (top-level area registry)
    └── areas/
        ├── 00-foundations.ayml
        ├── 01-linear-algebra.ayml
        ├── ...
        └── 16-gpu-programming.ayml
```

Source is [AYML](https://crates.io/crates/ayml), a safe serde-compatible
variant of YAML (triple-quoted multiline strings, no YAML footguns). The
loader is `src/graph.rs`, which compiles the whole graph into the binary at
build time via `include_dir!`. Point `--graph DIR` or `MT_GRAPH` at this
tree to run against a working copy instead of the embedded snapshot.

## Hierarchy vs prerequisites

Two distinct DAGs share the same nodes:

- **Taxonomic hierarchy** — atoms nest under clusters, clusters under
  areas. Implicit in the file's `children:` nesting. Used for navigation,
  grouping, and file layout.
- **Prerequisite graph** — "understand X before Y," encoded by each node's
  `prerequisites:` list. Crosses cluster and area boundaries freely. Used
  for ordering and reading-path generation. A prerequisite may point at a
  cluster, meaning "the whole cluster."

`mt graph check` validates both: id/prefix shape, prerequisite resolution,
cycles, and orphan atoms (a leaf nothing depends on must be marked
`terminal: true`).

## Lessons and quizzes

A node is **atomic** iff it has no `children`; atoms are mini-lessons (1–2
paragraphs, ≤ 2 minutes, ≤ 1 theorem / rule / definition). Nodes with
children are organizational **clusters**.

Most atoms ship as metadata only — `id`, `name`, `description`,
`prerequisites`. Lesson and quiz bodies are authored lazily at runtime: the
agent writes them on first presentation (`mt lesson upsert`, `mt quiz
create`) into the per-user overlay, after which they are deterministic. An
atom may also ship an inline `lesson:` body (and `quizzes:`) when it is
worth fixing canonically; the overlay overrides it. See the top-level
`docs/design.md` for the authoring lifecycle and `SCHEMA.md` for the field
reference.

## ID conventions

- Each area has a short prefix: `fnd`, `la`, `ana`, `prob`, `opt`, `dis`,
  `num`, `geom`, `dyn`, `inf`, `lt`, `nn`, `tx`, `dif`, `ssl`, `rl`, `gpu`.
- Node ID extends its parent by one positive-integer segment
  (`gpu` → `gpu.6` → `gpu.6.7`). Depth is unlimited; a 4-level path is
  normal.
- IDs are **stable** — renaming or relocating a concept keeps its ID.
- Deleted IDs are not reused.

## Status

Fully migrated to `schema_version: 2` (recursive `children:`, atomized
nodes) across every area, with descriptions and prerequisites filled out
throughout. Lesson/quiz bodies are filled in on demand at runtime rather
than shipped. New areas should be authored directly in v2 and validated
with `mt graph check`.
