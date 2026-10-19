# Math Tutor

Math curriculum manager for LLM-driven learning.

## Overview

Math Tutor manages a directed acyclic graph (DAG) of math concepts across LLM sessions and devices. Each atomic concept has a short lesson and quizzes. Quizzes are scheduled with spaced repetition via [FSRS](https://github.com/open-spaced-repetition/fsrs4anki).

The LLM generates lessons and quizzes on demand and grades user answers. Generated content is saved in a [Baserunna](https://baserunna.fly.dev) database so that subsequent reviews are deterministic.

## CLI

```
mt path list                          # list paths with goal and progress
mt path new <GOAL> --targets <ID>...  # create a learning path
mt path state [--path P]              # show path status summary
mt path next  [--path P]              # get the next scheduled action (AYML)
mt path tree  [--path P]              # show the reachable prerequisite tree

mt graph list [<ID>] [--path P]       # list areas or cluster children
mt graph show  <ID>  [--path P]       # show an atom, cluster, or area
mt graph check                        # validate the curriculum graph
mt graph dump                         # print the user overlay as AYML

mt lesson upsert <ATOM>  --body TEXT
mt quiz create <ATOM>    --difficulty D --question TEXT --answer TEXT \
                         [--rubric TEXT] [--type {free_text,multiple_choice}]
mt quiz update <QUIZ_ID> [--question TEXT] [--answer TEXT] [--rubric TEXT] \
                         [--difficulty D] [--type T]
mt quiz delete <QUIZ_ID>
mt quiz answer <QUIZ_ID> --rating {again,hard,good,easy} [--user-answer TEXT]

mt instruct                           # print the agent playbook
mt mcp                                # start the MCP HTTP server
```

See `docs/design.md` for architecture details.
