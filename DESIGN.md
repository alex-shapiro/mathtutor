# Math Tutor

Math Tutor is a tool for learning math via a DAG of small lessons and quizzes.
It incorporates spaced repetition learning to ensure that concepts remain learned.
It runs as CLI and stores all user data in text files, at least for now.

```bash
mt new <GOAL>             # start a new learning path to a goal
mt next [--path <PATH>]   # get the next node in an existing learning path (default: most recent)
mt state [--path <PATH>]  # get current status of a learning path (default: most recent)
mt graph check            # check/validate curriculum graph correctness
```

The most common command will be `mt next`. This will present one of:

- a new lesson in the learning path
- a quiz for the most recent lesson in the learning path
- a quiz for a previous lesson in the learning path (spaced repetition)

Each lesson is short, presenting 1 concept.
There should be 3 quizzes per lesson (easy, medium, hard).
Quiz questions must depend ONLY on knowledge learned from the current lesson and previous lessons. No "lookahead" questions.
Lessons and quizzes are created on demand and added to the appropriate section of the graph.

`mt` issues a log after every event

- mt amends the curriculum graph
- mt presents a lesson/quiz
- the user says they have learned a lesson
- the user answers a question
- the user skips a question
- the user asks to relearn a lesson
- the user asks for a hint to a question
- ...etc.

It uses this log to determine the next best lesson or quiz to present to the user.

All curriculum graph and learning paths are stored in [AYML](https://crates.io/crates/ayml) files. AYML safe and serde-compatible variant of YAML.
The only difference between AYML and YAML in normal use is that AYML uses triple-quote multiline string delimiters (like Swift) instead of `|\n`.
AYML also disallows YAML's long tail of fringe features that no one really uses.

All code is written in Rust. Crates:

- Use `argh` for CLI arg parsing
- Use `fsrs` for spaced repetition learning (not 100% on this, needs to be proved out)
- Use `tracing` for debug logging wherever needed
- Use `thiserror` for error types
- Use `serde` and `ayml` crates for data serialization
