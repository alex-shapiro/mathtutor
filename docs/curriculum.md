# Curriculum Authoring

This document defines the authoring rules for `curriculum/graph/areas/*.ayml`. The curriculum is a directed acyclic graph of atomic concepts grouped into clusters and areas.

## Atoms

Every atom must be a prerequisite for a downstream AI/ML concept, or set `terminal: true` as a leaf endpoint. Do not add duplicate atoms across areas. Place each concept in a single canonical location. Reference its ID from dependent nodes.

## Prerequisites

List only direct, required dependencies in `prerequisites`:

- Reference an **atom ID** if the dependent node requires a specific concept.
- Reference a **cluster ID** if the dependent node requires all concepts in that cluster.

A prerequisite of a cluster applies to every atom in that cluster. A prerequisite must not contain the node that lists it, because the node would then require itself. `mt graph check` rejects such a node.

Cross-area prerequisites are supported and encouraged.

## Quizzes

Give each curriculum quiz the ID `<atom>.q<N>`, where `N` is a positive integer. User quizzes use `<atom>.u<N>`, so this form keeps the two sets of IDs apart. `mt graph check` rejects a curriculum quiz ID in another form.

## Orphans

`mt graph check` reports any atom without `terminal: true` that is not referenced as a direct or cluster prerequisite. Resolve an orphan with these steps:

1. Add a prerequisite edge from the downstream consumer if a dependency was omitted.
2. Delete the atom if target topics in the curriculum do not require it.
3. Set `terminal: true` if the atom is an intentional leaf endpoint.

## Cycles

The prerequisite graph must be acyclic. If two concepts depend on each other, merge them into a single atom or split the foundational definition into a shared prerequisite.

## Validation

Validate changes against the working tree before committing:

```sh
mt graph check -p curriculum/graph
```
