# MCP Server Design

`mt mcp` exposes the Math Tutor tools and agent playbook over the Model Context Protocol using HTTP with Server-Sent Events.

## Architecture

- **Feature flag:** The `mcp` feature in `Cargo.toml` enables the HTTP server and `mt mcp` subcommand.
- **Transport:** Streamable HTTP and SSE via `axum` and `rmcp`.

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

1. **Static bearer token:** Configured via `MT_API_KEY` and passed in `Authorization: Bearer <token>` or `?token=<token>`. The key must have at least 32 characters. Use a random key, such as the output of `openssl rand -base64 32`.
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

### Token parameters and storage

- **Entropy:** 256-bit random tokens encoded with base64url.
- **Expirations:** Single-use authorization codes expire in 60 seconds. Access tokens expire in 1 hour. Refresh tokens expire in 30 days and rotate on each refresh.
- **PKCE:** `S256` code challenge is mandatory.
- **Scope:** The server grants only the `mcp` scope. An authorization request without a scope receives `mcp`. A request for another scope fails with `invalid_scope`. A refresh request can keep or narrow the scope of its grant. The server accepts only an access token whose scope includes `mcp`.
- **Client registrations:** Registration is open, because MCP connectors register themselves. The server deletes a client 1 day after registration if the client gets no token. Each code exchange and refresh keeps the client for 30 more days, the lifetime of the new refresh token.
- **Tables:** `oauth_clients`, `oauth_codes`, `oauth_access_tokens`, `oauth_refresh_tokens`, and `oauth_addr_counts` are stored in the Baserunna database using native row TTLs for expiration. Code and token rows are keyed by the SHA-256 digest of the secret, so the database does not hold usable bearer secrets.
- **Sign-in form:** The form shows the name that the client registered and the host of its redirect URI, which receives the authorization code. Registration is open, so the owner must check both before signing in. The CSRF token of the form expires after 15 minutes. Its signing key derives from `BASERUNNA_TOKEN`, so a form stays valid after a restart and on every machine of the app. A change of `BASERUNNA_TOKEN` makes open forms expire. If a form expired, the server shows a new form and asks for the password again.

### Limits per client address

- **Failed credentials:** Incorrect passwords and failed bearer tokens on `/mcp` count against one limit. After 5 failures from an address, each next attempt must wait. The wait starts at 1 second and doubles after each further failure, up to 15 minutes. Thus an address can try about 17 credentials in the first hour and 4 in each later hour. The server forgets the failures of an address after a correct password or access token, or 1 hour after the last failure. A delayed attempt receives `429 Too Many Requests` with a `Retry-After` header, and the server does not check its credential. A cached access token does not wait.
- **Registration:** An address can register 10 clients per hour. More registrations receive `429 Too Many Requests` with a `Retry-After` header.
- **Client address:** The server uses the TCP peer address. Behind a proxy, set `MT_CLIENT_IP_HEADER` or `--client-ip-header` to the header that the proxy sets to the client address. Use only a header that the proxy replaces, because a client can send any header. `fly.toml` sets `Fly-Client-IP`. All IPv6 addresses of one `/64` network share their limits.
- **Storage:** The server stores the count of each address in `oauth_addr_counts`, so the limits stay the same after a restart and on every machine of the app. Each server also counts in memory, for at most 10,000 addresses, so a burst of requests reaches the database only for the attempts that the limits admit. A failed token without the form of an access token counts only in memory.

## Deployment

`Dockerfile` and `fly.toml` deploy `mt mcp` to Fly.io via `.github/workflows/deploy-mcp.yml`.

Required secrets:

```sh
flyctl secrets set MT_ADMIN_PASSWORD='...'
flyctl secrets set MT_API_KEY='...'
flyctl secrets set BASERUNNA_TOKEN='...'
```

`BASERUNNA_URL` defaults to `https://baserunna.fly.dev` and `BASERUNNA_DB` defaults to `mathtutor`. Set `BASERUNNA_TOKEN` to a key that is limited to the database, so that a leak of the server secret cannot delete the database.

Give the server key its own rate limit below the limit of the Baserunna user. Baserunna lets each user send 500 requests per second on each node, through all of their keys together. Without a lower limit on the server key, a flood of requests to the server can use the full budget and slow or block the CLI on every device. The web console cannot set the limit of a key, so create the server key through the API with a session token:

```sh
curl -X POST "$BASERUNNA_URL/v1/auth/login" -H 'Content-Type: application/json' \
  -d '{"email": "...", "password": "..."}'   # returns a session token
curl -X POST "$BASERUNNA_URL/v1/api-keys" -H "Authorization: Bearer <session token>" \
  -H 'Content-Type: application/json' \
  -d '{"name": "mt mcp", "db": "mathtutor", "rate_limit": 50}'
```

The `rate_limit` value is the number of requests per second on each Baserunna node. The server does not create the database. It stops at startup if the database does not exist. See "Baserunna keys" in `docs/design.md`.
