# MCP Server Design

`mt mcp` exposes the Math Tutor tools and agent playbook over the Model Context Protocol using HTTP with Server-Sent Events.

## Architecture

- **Feature flag:** The `mcp` feature in `Cargo.toml` enables the HTTP server and `mt mcp` subcommand.
- **Transport:** Streamable HTTP and SSE via `axum` and `rmcp`.
- **Sessions:** The server keeps no MCP session. Each tool takes the path that it acts on, so any server process can answer any request, also after a restart or on a different machine of the app.

## Interface

### Tool conventions

- **`path_id`**:
  - Required for all path-scoped tools.
  - Optional for the curriculum inspection tools `get_item` and `get_children`. If omitted, those tools return the curriculum with the authored lessons and quizzes, without path completion state. Unlike `mt graph show` and `mt graph list`, they always read the overlay, because the server always has the database.
- **Return format**: All tools return JSON. A read tool returns the same record as the matching CLI command: `get_paths` as `mt path list`, `get_state` as `mt path state`, `get_tree` as `mt path tree`, `get_syllabus` as `mt path syllabus`, `get_item` as `mt graph show`, and `get_children` as `mt graph list`. `get_next` returns the `mt path next` record, and its `next_step` is an MCP tool call. Business logic errors return a `CallToolResult` with `isError: true` and a JSON object with `error` and `kind`. The error kinds are listed in "Tool I/O and exit codes" in `docs/design.md`. If the commit status is unknown, `kind` is `outcome_unknown`, and `next_step` gives the safe next call.

### Prompt

The `mathtutor-playbook` MCP prompt returns the agent instructions for operating the MCP tools.

## Authentication and security

The server operates as a single-user, single-tenant instance backed by a single Baserunna database. Two authentication modes are supported on `/mcp`:

1. **Static bearer token:** Configured via `MT_API_KEY` and passed in `Authorization: Bearer <token>` or `?token=<token>`. The server accepts the `?token=` form only for `MT_API_KEY`, because proxies and servers log URLs. The key must have at least 32 characters. Use a random key, such as the output of `openssl rand -base64 32`.
2. **OAuth 2.1 access token:** Issued by the built-in authorization server if `MT_ADMIN_PASSWORD` is set, and passed in `Authorization: Bearer <token>`.

Either `MT_API_KEY` or `MT_ADMIN_PASSWORD` must be set at startup. Unauthenticated requests receive `401 Unauthorized` with a `WWW-Authenticate` header pointing to `/.well-known/oauth-protected-resource`. The server keeps each valid access token in memory until it expires, so it reads a token from Baserunna once. The server has no revocation endpoint, so the cache does not extend the life of a token. If the server cannot read an access token from Baserunna, the request receives `503 Service Unavailable` with a `Retry-After` header. A token without the form of an issued access token fails without a Baserunna request.

### Built-in OAuth 2.1 authorization server

| Path                                      | Method | Purpose                                                |
| ----------------------------------------- | ------ | ------------------------------------------------------ |
| `/.well-known/oauth-protected-resource`   | GET    | RFC 9728 protected resource metadata                   |
| `/.well-known/oauth-authorization-server` | GET    | RFC 8414 authorization server metadata                 |
| `/oauth/register`                         | POST   | RFC 7591 dynamic client registration                   |
| `/oauth/authorize`                        | GET    | Renders the login form with a signed CSRF token       |
| `/oauth/authorize`                        | POST   | Verifies `MT_ADMIN_PASSWORD` and issues an auth code   |
| `/oauth/token`                            | POST   | Exchanges an auth code with S256 PKCE or refresh token |

Only the metadata routes, `/oauth/register`, and `/oauth/token` send CORS headers, so a browser-based MCP client on another origin can use them. `/oauth/authorize` sends no CORS headers, so another website cannot read the sign-in form.

### Token parameters and storage

- **Entropy:** 256-bit random tokens encoded with base64url.
- **Expirations:** Single-use authorization codes expire in 60 seconds. Access tokens expire in 1 hour. Refresh tokens expire in 30 days and rotate on each refresh.
- **PKCE:** `S256` code challenge is mandatory.
- **Scope:** The server grants only the `mcp` scope. An authorization request without a scope receives `mcp`. An authorization or refresh request for another scope fails with `invalid_scope`.
- **Client registrations:** Registration is open, because MCP connectors register themselves. The server deletes a client 1 day after registration if the client gets no token. Each code exchange and refresh keeps the client for 30 more days, the lifetime of the new refresh token.
- **Tables:** The server stores its OAuth state in the Baserunna database, in `oauth_clients`, `oauth_codes`, `oauth_access_tokens`, `oauth_refresh_tokens`, `oauth_addr_counts`, and `oauth_keys`. Each row except the `oauth_keys` rows expires through a native row TTL. Code and token rows are keyed by the SHA-256 digest of the secret, so the database does not hold usable bearer secrets.
- **Sign-in form:** The form shows the name that the client registered and the host of its redirect URI, which receives the authorization code. Registration is open, so the owner must check both before signing in. The CSRF token of the form expires after 15 minutes. Its signing key is a random key that the server stores in the `oauth_keys` table on first use, so a form stays valid after a restart, after a change of `BASERUNNA_TOKEN`, and on every machine of the app. If a form expired, the server shows a new form and asks for the password again. Each response of `/oauth/authorize` sends `Content-Security-Policy: frame-ancestors 'none'` and `X-Frame-Options: DENY`, so no site can show the form in a frame.

### Limits per client address

- **Incorrect passwords:** After 5 incorrect passwords from an address, each next sign-in must wait. The wait starts at 1 second and doubles after each further failure, up to 15 minutes. Thus an address can try about 17 passwords in the first hour and 4 in each later hour. The server forgets the failures of an address after a correct password, or 1 hour after the last failure. A delayed sign-in receives `429 Too Many Requests` with a `Retry-After` header, and the server does not check its password.
- **Rejected bearer tokens:** After 20 rejected bearer tokens on `/mcp` from an address in 1 minute, each next bearer token from the address receives `429 Too Many Requests` with a `Retry-After` header until the minute ends. This limit is separate from the password limit, so failed tokens from a shared address, such as the egress address of a hosted connector, cannot delay a sign-in. `MT_API_KEY` and a cached access token never wait. Each server counts rejected tokens only in memory.
- **Registration:** An address can register 10 clients per hour. More registrations receive `429 Too Many Requests` with a `Retry-After` header.
- **Sign-in and token requests:** An address can send 60 requests per minute to `/oauth/authorize` and `/oauth/token` together, because each request reads the database. More requests receive `429 Too Many Requests` with a `Retry-After` header before the server reads the database. Each server counts these requests only in memory.
- **Client address:** The server uses the TCP peer address. Behind a proxy, set `MT_CLIENT_IP_HEADER` or `--client-ip-header` to the header that the proxy sets to the client address. Use only a header that the proxy replaces, because a client can send any header. `fly.toml` sets `Fly-Client-IP`. All IPv6 addresses of one `/64` network share their limits.
- **Storage:** The server stores the password and registration counts of each address in `oauth_addr_counts`, so the limits stay the same after a restart and on every machine of the app. Each server also counts in memory, for at most 10,000 addresses, so a burst of requests reaches the database only for the attempts that the limits admit.

## Deployment

`Dockerfile` and `fly.toml` deploy `mt mcp` to Fly.io via `.github/workflows/deploy-mcp.yml`.

### Health checks

- **`/health`:** Returns `200 ok` without a Baserunna request. The server starts and answers `/health` before its first Baserunna request. The Fly service check uses it, so a Baserunna outage does not take the server out of routing or make Fly restart it, also if the outage starts while the machine is stopped.
- **`/ready`:** Reads the state of the database and returns `200 ready` if the tools can use it. Otherwise it returns `503` with the error kind, such as `{"ready": false, "kind": "config"}` for a missing database or a rejected key. If the database does not answer in 5 seconds, it returns `503` with the `database` kind. The server reuses each result for 10 seconds, and runs one check at a time, so unauthenticated requests to `/ready` send at most one Baserunna request in each interval. The top-level check in `fly.toml` and the last step of the deploy workflow use it, so a deployment that cannot use its database fails.

Neither route needs authentication.

Required secrets:

```sh
flyctl secrets set MT_ADMIN_PASSWORD='...'
flyctl secrets set MT_API_KEY='...'
flyctl secrets set BASERUNNA_TOKEN='...'
```

`MT_API_KEY` must have at least 32 characters, or the server stops at startup. Use a random key, such as the output of `openssl rand -base64 32`. To rotate the key, set the new value with `flyctl secrets set`, which restarts the machines, and then give the new key to each client that uses it. The old key stops working at the restart.

`fly.toml` sets `BASERUNNA_URL` in its `[env]` section, so the server does not depend on the default URL of `mt`. `BASERUNNA_DB` defaults to `mathtutor`. Set `BASERUNNA_TOKEN` to a key that is limited to the database, so that a leak of the server secret cannot delete the database.

Give the server key its own rate limit below the limit of the Baserunna user. Baserunna lets each user send 500 requests per second on each node, through all of their keys together. Without a lower limit on the server key, a flood of requests to the server can use the full budget and slow or block the CLI on every device. The web console cannot set the limit of a key, so create the server key through the API with a session token:

```sh
curl -X POST "$BASERUNNA_URL/v1/auth/login" -H 'Content-Type: application/json' \
  -d '{"email": "...", "password": "..."}'   # returns a session token
curl -X POST "$BASERUNNA_URL/v1/api-keys" -H "Authorization: Bearer <session token>" \
  -H 'Content-Type: application/json' \
  -d '{"name": "mt mcp", "db": "mathtutor", "rate_limit": 50}'
```

The `rate_limit` value is the number of requests per second on each Baserunna node. The server does not create the database. If the database does not exist, the server still starts, `/ready` fails with the `config` kind, and each tool call fails with an error that names `BASERUNNA_DB`. See "Baserunna keys" in `docs/design.md`.
