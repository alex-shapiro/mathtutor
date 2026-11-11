# Math Tutor

Math curriculum manager for LLM-driven learning.

## Overview

Math Tutor manages a directed acyclic graph of math concepts across LLM sessions and devices. Each atomic concept has a short lesson and quizzes. Quizzes are scheduled with spaced repetition via [FSRS](https://github.com/open-spaced-repetition/fsrs4anki).

The LLM generates lessons and quizzes on demand and grades user answers. Generated content is saved in a [Baserunna](https://baserunna.fly.dev) database so that subsequent reviews are deterministic.

## Setup

`mt` stores learner state in Baserunna and requires network access. Follow these steps to get a server and an API key:

1. Run a Baserunna server. For a local node, run `cargo run --release` in a clone of [Baserunna](https://github.com/alex-shapiro/baserunna). To host a server, follow its `DEPLOYING.md`. The default `BASERUNNA_URL` points to the Math Tutor maintainer's deployment, so set `BASERUNNA_URL` to your own server.
2. Sign up in the web console of the server at `/console`.
3. Create an API key on the keys page of the console.

Set these environment variables before you run a command that reads or writes learner state:

- `BASERUNNA_TOKEN`: required API key.
- `BASERUNNA_DB`: database name. Defaults to `mathtutor`.
- `BASERUNNA_URL`: server URL. Defaults to `https://baserunna.fly.dev`.

Create the database in either of two ways:

- **In the console:** Click "Create database" in the console with the name from `BASERUNNA_DB`. Next, create a key limited to this database for each device and server. The first command creates the tables.
- **With `mt`:** The first `mt path new` or `mt import-turso` creates the database. Only an unrestricted key can create or delete a database. After the first run, give each device and server its own database-limited key.

See "Baserunna keys" in `docs/design.md`.

`mt instruct`, `mt graph check`, and `mt mcp tools` run without these variables. `mt graph list` and `mt graph show` also run without them if `--path` is omitted.

`mt` uses the curriculum embedded in the binary by default. Pass `--graph DIR` before the command, as in `mt --graph DIR path next`, or set `MT_GRAPH` to use a curriculum directory instead.

To migrate an existing libSQL database, run `mt import-turso` before creating a path. See `docs/design.md` for details. To migrate AYML state files, run `mt migrate-from-ayml` from a libSQL release first, and then run `mt import-turso`.

## CLI

```
mt path list                          # list paths with goal and progress
mt path new <GOAL> --targets <ID>... [--strategy {bottom_up,top_down}]
                                      # create a learning path
mt path state [--path P]              # show path status summary
mt path next  [--path P] [--new | --due]
                                      # get the next scheduled action as AYML
mt path syllabus [--path P] [-n N]    # list upcoming untaught lesson topics
mt path tree  [--path P] [--depth N]  # show the reachable prerequisite tree
mt path strategy {bottom_up,top_down} [--path P]
                                      # change the traversal strategy
mt path subpath set --atoms <ID>... [--path P]
                                      # teach these atoms first in top_down mode
mt path subpath clear [--path P]      # remove the subpath

mt graph list [<ID>] [--path P] [--recursive]  # list areas or cluster children
mt graph show  <ID>  [--path P]       # show an atom, cluster, or area
mt graph check [-p DIR]               # validate the curriculum graph
mt graph dump                         # print the user overlay as AYML

mt lesson upsert <ATOM>  --body TEXT [--path P]
mt quiz create <ATOM>    --difficulty D --question TEXT --answer TEXT \
                         [--rubric TEXT] [--type {free_text,multiple_choice}] [--path P]
mt quiz update <QUIZ_ID> [--question TEXT] [--answer TEXT] [--rubric TEXT] \
                         [--difficulty D] [--type T] [--path P]
mt quiz delete <QUIZ_ID> [--path P]
mt quiz answer <QUIZ_ID> --rating {again,hard,good,easy} [--user-answer TEXT] [--path P]

mt instruct                           # print the agent playbook
mt mcp                                # start the MCP HTTP server; see docs/mcp.md
mt mcp tools                          # print the MCP tool schemas as JSON
mt import-turso [--url URL]           # import a libSQL database into Baserunna

mt --graph DIR <COMMAND>              # use the curriculum in DIR
```

Put each `TEXT` value in single quotes. Write each single quote in the text as `'\''`. The shell then passes `$`, `\`, `"`, and line breaks to `mt` unchanged.

See `docs/design.md` for architecture details.
