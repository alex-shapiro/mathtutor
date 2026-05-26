# Math Tutor

Overall design doc: docs/design.md

## Dev Workflow

- check out a branch from main ToT
- implement feature changes
- ensure each feature is tested with specific, concrete assertions based on the needs of the feature
- add integration tests where appropriate
- run `cargo clippy` and fix all lints. Cargo.toml is already configured to surface pedantic lints.
- run `cargo fmt` to format idiomatically.
- when everything is complete, commit and push branch to origin
- open a PR with `gh` and ask for a review

## Guidelines

- You are building production software, not a prototype.
- Prioritize correctness, performance, and long-term maintainability.
- Avoid simplifying assumptions and "good enough for now" work.
- Use best practices from the SOTA literature whenever possible.
- All code must be concise, maintainable, and tested.
- Doc and module comments are concise descriptions of the API contract with the caller.
  Avoid changelogs or implementation details unless pertinent to the caller.
- Code comments provide "what and why" details for unintuitive implementations.
  Avoid summaries of obvious code or changelogs.
- The best code is concise and the best comments are 1 line.
  Beyond that, the reader tends to tune out and mainenance becomes more difficult.
- Commit messages and PR descriptions must be human readable and concise.
  They describe the motivation for and essence of a change.
  They do not rehash implementation details or specific symbols from the diff.
