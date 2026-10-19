# Curriculum Authoring

This document defines the authoring rules for `curriculum/graph/areas/*.ayml`. The curriculum is a directed acyclic graph (DAG) of atomic concepts grouped into clusters and areas.

## Atoms

Every atom must be a prerequisite for a downstream AI/ML concept, or be explicitly marked as a terminal endpoint (`terminal: true`). Do not add duplicate atoms across areas; place each concept in a single canonical location and reference its ID from dependent nodes.

## Prerequisites

List only direct, required dependencies in `prerequisites`:

- Reference an **atom ID** when the dependent node requires a specific concept.
- Reference a **cluster ID** when the dependent node requires all concepts in that cluster.

Cross-area prerequisites are supported and encouraged.

## Orphans

`mt graph check` reports any atom that is not referenced as a prerequisite (directly or through an ancestor cluster) and is not marked with `terminal: true`. To resolve an orphan:

1. Add a prerequisite edge from the downstream consumer if a dependency was omitted.
2. Delete the atom if it is not required by any target topic in the curriculum.
3. Set `terminal: true` if the atom is an intentional leaf endpoint.

## Cycles

The prerequisite graph must be acyclic. If two concepts depend on each other, merge them into a single atom or split the foundational definition into a shared prerequisite.

## Validation

Validate changes against the working tree before committing:

```sh
mt graph check -p curriculum/graph
```
