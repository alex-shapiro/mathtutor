# Tradeoffs

Open product decisions from the branch review. Each section states the problem and the options.

## A lost refresh response signs the connector out

A refresh request to `/oauth/token` deletes the presented refresh token in the transaction that issues the new pair. If the response does not reach the connector, the connector holds only the deleted token, and its next refresh fails. The user must sign in again.

- **Keep the old token valid for a short grace window:** A lost response has zero effect. A stolen refresh token stays usable for longer.
- **Return the same new pair if the old token comes back within the window:** A lost response has zero effect, and the server issues no extra secret. A stolen refresh token has the same longer exposure.
- **Keep strict rotation:** A stolen token works only once. A lost response signs the connector out.

## Who creates the database

The first `mt path new` creates the database. Only a key without a database limit can create a database, and such a key can also delete every database of its user. Thus the first session of an agent must hold a key that can delete all learner history.

- **Add an operator setup command, such as `mt db create`:** The operator runs it once with a temporary unlimited key. After that, every learner command uses a key that is limited to the database. A new user has an extra setup step.
- **Keep creation in `mt path new`:** Setup has no extra step. The agent holds an unlimited key on first use.

The current behavior also blocks the first agent session. The playbook starts with `mt path state`, which fails on a new setup because the database does not exist yet. The error tells the agent to check `BASERUNNA_DB` and `BASERUNNA_URL`, so the agent stops before it reaches `mt path new`. A user of MCP only must create a path from the CLI before `mt mcp` can start. The setup command removes both problems. A creation hint in the error removes them too, but an agent that follows the hint after a typo in `BASERUNNA_DB` starts a second, empty learner history.

## Devices as full database clients

The CLI on each device and the MCP server are full clients of the database. Each one holds a Baserunna key, writes the derived `cards` and `taught` rows with its own FSRS version, checks the schema version, and downloads the full overlay after each overlay edit. Four items in `TODO.md` come from this cause: a leaked device key can drop tables, every device must run the same release, devices on different releases compute cards differently, and each device downloads the full overlay.

- **Make the CLI a client of the server API:** Only the server holds a Baserunna key, writes projections, and checks the schema. Each device holds a token that the server can revoke and that cannot drop tables. The CLI needs a running `mt mcp` server, a Fly cold start delays the first command, and a user with one device must run or host a server.
- **Keep devices as full clients:** No server is necessary. The four problems stay, and each one needs its own fix.

## Sign-in delay after incorrect passwords

`/oauth/authorize` delays the next attempt from an address after 5 incorrect passwords. The wait doubles from 1 second up to a limit of 15 minutes, so an attacker at the limit can try about 4 passwords per hour from each address. The limit also applies to the administrator on the same address, and to every user behind the same NAT or `/64` IPv6 network. The server uses 15 minutes now.

- **Shorter limit, such as 1 minute:** A user who mistypes many times waits less. An attacker can try about 60 passwords per hour from each address.
- **Longer limit, such as 1 hour:** An attacker can try 1 password per hour from each address. A user who mistypes many times can wait up to 1 hour.
- **Add a global limit on failures from all addresses:** An attacker with many addresses gets few attempts. An attacker can lock out the administrator from every address.

## Default Baserunna server

`BASERUNNA_URL` defaults to `https://baserunna.fly.dev`, the deployment of the maintainer. A user who does not set `BASERUNNA_URL` sends the API key of another server to this deployment. The deployment rejects the key with a 401, and `mt` reports the URL and the two settings to check.

- **Remove the default and fail with exit code `2` that names `BASERUNNA_URL`:** No key goes to an unintended server. Each user sets one more variable.
- **Keep the default as a hosted service and state its limits in the README:** Setup stays short. Users depend on a single node without backup that the maintainer operates.
- **Keep the default only for the maintainer and document it:** Setup stays short for the maintainer. Other users can send their key to the maintainer's server by mistake.
