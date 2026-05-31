# Math Tutor

LLM-driven math tutor with a CLI and MCP server.

## CLI surface

Subcommands are resource-first (`mt <noun> <verb>`) and track the MCP
tool surface 1:1.

```
mt path list                       # list every path with goal / progress
mt path new <GOAL> --atom <ID>...  # start a new learning path
mt path state [--path P]           # one-screen status summary
mt path next  [--path P]           # next scheduled action (AYML on stdout)
mt path tree  [--path P]           # full reachable-graph progress view

mt graph list [<ID>] [--path P]    # browse areas / cluster children
mt graph show  <ID>  [--path P]    # detail on an atom, cluster, or area
mt graph check                     # validate the shipped curriculum
mt graph dump                      # print the user overlay AYML

mt lesson upsert <ATOM>    --body TEXT
mt quiz create <ATOM>    --difficulty D --question TEXT --answer TEXT \
                         [--rubric TEXT] [--type {free_text,multiple_choice}]
mt quiz update <QUIZ_ID> [--question TEXT] [--answer TEXT] [--rubric TEXT] \
                         [--difficulty D] [--type T]
mt quiz delete <QUIZ_ID>
mt quiz answer <QUIZ_ID> --rating {again,hard,good,easy} [--user-answer TEXT]

mt instruct                        # print the agent operator playbook
mt mcp                             # run the MCP server (SSE over HTTP)
```

See `docs/design.md` for the full design and `mt instruct` for the
embedded agent playbook.
