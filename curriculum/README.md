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

The curriculum files use [AYML](https://crates.io/crates/ayml). The build embeds them into the binary via `include_dir!`. Use `--graph DIR` or `MT_GRAPH` to load an external directory at runtime.

## Hierarchy and prerequisites

The graph defines two relationships over the same nodes:

- **Taxonomic hierarchy:** Atoms are nested under clusters, and clusters are nested under areas via `children:`.
- **Prerequisite DAG:** Dependencies are listed in `prerequisites:` and can reference atoms or clusters across any area.

`mt graph check` validates ID formatting, prerequisite resolution, acyclicity, and orphan status. Any leaf atom without dependents must set `terminal: true`.

## Lessons and quizzes

A node is an **atom** if `children` is omitted or empty; otherwise, it is a **cluster**.

Most atoms in the repository contain only metadata. The agent authors lesson and quiz bodies on demand at runtime and stores them in the user overlay. Fixed canonical content can also be defined inline in the AYML file.

## ID conventions

- Each area has a short prefix defined in `manifest.ayml`.
- Each child ID appends a positive integer segment to its parent ID, such as `gpu.6.7` under `gpu.6`.
- IDs are permanent and are never reused after deletion.
