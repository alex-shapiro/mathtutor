# Math Tutor

Math curriculum manager for LLM-driven learning.

## Overview

Math Tutor manages a directed acyclic graph (DAG) of math concepts across LLM sessions and devices. Each atomic concept has a short lesson and quizzes. Quizzes are scheduled with spaced repetition via [FSRS](https://github.com/open-spaced-repetition/fsrs4anki).

The LLM generates lessons and quizzes on demand and grades user answers. Generated content is saved in a [Baserunna](https://baserunna.fly.dev) database so that subsequent reviews are deterministic.

## Setup

`mt` stores learner state in Baserunna and requires network access. To get a server and an API key:

1. Run a Baserunna server. For a local node, run `cargo run --release` in a clone of [Baserunna](https://github.com/alex-shapiro/baserunna). To host a server, follow its `DEPLOYING.md`. The default `BASERUNNA_URL` is the deployment of the Math Tutor maintainer, so set `BASERUNNA_URL` to your own server.
2. Sign up in the web console of the server at `/console`.
3. Create an API key on the keys page of the console.

Set these environment variables before you run a command that reads or writes learner state:

- `BASERUNNA_TOKEN`: required API key.
- `BASERUNNA_DB`: database name (default: `mathtutor`).
- `BASERUNNA_URL`: server URL (default: `https://baserunna.fly.dev`).

The first `mt path new` creates the database. Only a key without a database limit can create a database, and that key can also delete it. After the first run, give each device and server its own key that is limited to the database. See "Baserunna keys" in `docs/design.md`.

`mt graph list` and `mt graph show` run without these variables if `--path` is omitted.

`mt` uses the curriculum that is embedded in the binary. To use a curriculum directory instead, pass `--graph DIR` before the command, as in `mt --graph DIR path next`, or set `MT_GRAPH`.

To migrate an existing libSQL database, run `mt import-turso` before creating a path. See `docs/design.md` for details. `mt migrate-from-ayml` no longer exists. To migrate AYML state files, run `mt migrate-from-ayml` from a libSQL release first, and then run `mt import-turso`.

## CLI

```
mt path list                          # list paths with goal and progress
mt path new <GOAL> --targets <ID>... [--strategy {bottom-up,top-down}]
                                      # create a learning path
mt path state [--path P]              # show path status summary
mt path next  [--path P] [--new | --due]
                                      # get the next scheduled action (AYML)
mt path syllabus [--path P] [-n N]    # list upcoming untaught lesson topics
mt path tree  [--path P] [--depth N]  # show the reachable prerequisite tree
mt path strategy {bottom-up,top-down} [--path P]
                                      # change the traversal strategy
mt path subpath set --atoms <ID>... [--path P]
                                      # teach these atoms first in top-down mode
mt path subpath clear [--path P]      # remove the subpath

mt graph list [<ID>] [--path P] [--recursive]  # list areas or cluster children
mt graph show  <ID>  [--path P]       # show an atom, cluster, or area
mt graph check [-p DIR]               # validate the curriculum graph
mt graph dump                         # print the user overlay as AYML

mt lesson upsert <ATOM>  --body TEXT
mt quiz create <ATOM>    --difficulty D --question TEXT --answer TEXT \
                         [--rubric TEXT] [--type {free_text,multiple_choice}]
mt quiz update <QUIZ_ID> [--question TEXT] [--answer TEXT] [--rubric TEXT] \
                         [--difficulty D] [--type T]
mt quiz delete <QUIZ_ID>
mt quiz answer <QUIZ_ID> --rating {again,hard,good,easy} [--user-answer TEXT]

mt instruct                           # print the agent playbook
mt mcp                                # start the MCP HTTP server (see docs/mcp.md)
mt import-turso [--url URL]           # import a libSQL database into Baserunna

mt --graph DIR <COMMAND>              # use the curriculum in DIR instead of the embedded one
```

Put each `TEXT` value in single quotes, and write each single quote in the text as `'\''`. The shell then passes `$`, `\`, `"`, and line breaks to `mt` unchanged.

See `docs/design.md` for architecture details.
