# Math Tutor Curriculum Graph

A hierarchical, prerequisite-ordered concept graph covering the mathematics and computer science required for deep learning research.

## Layout

```
curriculum/
├── README.md            (overview)
├── SCHEMA.md            (schema specification)
└── graph/
    ├── manifest.ayml    (top-level area registry)
    └── areas/
        ├── 00-foundations.ayml
        ├── 01-linear-algebra.ayml
        ├── ...
        └── 16-gpu-programming.ayml
```

The curriculum files use [AYML](https://crates.io/crates/ayml) and are compiled into the binary via `include_dir!`. Use `--graph DIR` or `MT_GRAPH` to load an external directory at runtime.

## Hierarchy and prerequisites

The graph defines two relationships over the same nodes:

- **Taxonomic hierarchy:** Atoms are nested under clusters, and clusters are nested under areas via `children:`.
- **Prerequisite DAG:** Dependencies are listed in `prerequisites:` and can reference atoms or clusters across any area.

`mt graph check` validates ID formatting, prerequisite resolution, acyclicity, and orphan status (any leaf atom without dependents must set `terminal: true`).

## Lessons and quizzes

A node is an **atom** if `children` is omitted or empty; otherwise, it is a **cluster**.

Most atoms in the repository contain only metadata (`id`, `name`, `description`, `prerequisites`). Lesson and quiz bodies are authored on demand at runtime and stored in the user overlay, or defined inline in the AYML file when fixed canonically.

## ID conventions

- Each area has a short prefix (`fnd`, `la`, `ana`, `prob`, `opt`, `dis`, `num`, `geom`, `dyn`, `inf`, `lt`, `nn`, `tx`, `dif`, `ssl`, `rl`, `gpu`).
- Each child ID appends a positive integer segment to its parent ID (for example, `gpu` -> `gpu.6` -> `gpu.6.7`).
- IDs are permanent and are never reused after deletion.
