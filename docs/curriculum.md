# Curriculum authoring

Rules of thumb for editing `curriculum/graph/areas/*.ayml`. The shipped curriculum is a DAG over atoms (leaf concepts) grouped into clusters and areas; prerequisites are the only edges. The goal is fidelity to _how a practitioner actually builds on prior concepts in AI/ML_, not encyclopedic coverage of mathematics.

## Atoms

An atom should exist because some other atom (in this or another area) builds on it. Before adding or keeping an atom, ask: **what AI/ML concept transitively depends on this?** If you can't name one, it's either filler (delete) or a documented terminal endpoint (`terminal: true`). Encyclopedic completeness is not a goal — completeness _along the paths a DL practitioner walks_ is.

Avoid duplicates. If an atom's description is "Cross-link to la.X.Y", it's a sign the topology is wrong: pick one home, delete the other, and let downstream cite the canonical one.

## Prerequisites

A prerequisite is a load-bearing dependency, not an associative or motivational link. Negative results, motivating examples, and "good to know first" facts that the downstream doesn't actually use don't belong as prereqs.

**Atom** prereq when the downstream needs one specific fact. **Cluster** prereq when the downstream builds on a whole topic. If "PLU partial pivoting" needs all permutation-matrix theory (definition, sign, det, group structure), the natural prereq is `la.2.7`, not `la.2.7.1`. The scheduler and orphan check both expand cluster prereqs into all descendants.

Wire across areas freely. The natural consumer of a foundational atom often lives upstream by chapter but downstream by dependency — e.g. Markov chain stationary distributions (in `prob`) are the load-bearing use of general eigendecomposition (in `la.5.6`). Linear algebra in particular is foundational for almost every higher area; expect many cross-area prereqs from `opt`, `num`, `nn`, `tx`, `prob`, `dyn`, `geom`, `ana`.

## Orphans

`mt graph check` flags atoms that no other concept lists as a prerequisite (directly or via an ancestor cluster). An orphan signals one of three things:

1. **Missing wiring** if a real downstream consumer exists but doesn't cite this atom. Add the prereq.
2. **Atom doesn't belong** if no path to any AI/ML concept exists. Delete.
3. **Genuinely culminating** if the atom is a known endpoint with no further consumer in this curriculum. Mark `terminal: true`.

Case 3 should be rare in foundational areas (`la`, `ana`, `prob`, `opt`, `fnd`). Most of what looks terminal in those areas actually has a consumer somewhere higher — when in doubt, search the higher-area files for the topic by name before reaching for `terminal: true`. Terminal is more natural for application-layer leaves (`tx`, `nn`, `dif`, `ssl`, `rl`) where the concept genuinely is the end of a chain.

## Cycles

The DAG must remain acyclic. `mt graph check` detects cycles; do not introduce them. If two atoms genuinely need each other, the topology is wrong. One is likely mis-classified, or else they should be merged.

## Tooling

While editing AYML, run the validator against the live tree, not the embedded copy:

```
mt graph check -p curriculum/graph
```

`include_dir!` does not always rebuild on AYML-only changes, so plain `mt graph check` can lag the file system and give stale orphan counts.
