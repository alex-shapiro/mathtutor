# MCP Server Design

`mt mcp` exposes the Math Tutor tools and agent playbook over the Model Context Protocol using HTTP with Server-Sent Events.

## Architecture

- **Feature flag:** The `mcp` feature in `Cargo.toml` enables the HTTP server and `mt mcp` subcommand.
- **Transport:** Streamable HTTP and SSE via `axum` and `rmcp`.
- **Sessions:** The server is stateless and does not keep MCP sessions. Each tool takes its target path ID, so any server process can answer any request across restarts and machines.

## Interface

### Tool conventions

- **`path_id`**:
  - Required for all path-scoped tools.
  - Optional for the curriculum inspection tools `get_item` and `get_children`. If omitted, those tools return the curriculum merged with the overlay, without path completion state.
- **Return format**: All tools return JSON:
  - Read tools return the same record as the matching CLI command: `get_paths` matches `mt path list`, `get_state` matches `mt path state`, `get_tree` matches `mt path tree`, `get_syllabus` matches `mt path syllabus`, `get_item` matches `mt graph show`, and `get_children` matches `mt graph list`.
  - `get_next` returns the `mt path next` record with an MCP tool call in `next_step`.
  - Business logic errors return a `CallToolResult` with `isError: true` and a JSON object containing `error` and `kind`. See "Tool I/O and exit codes" in `docs/design.md` for the error kinds.
  - If the commit status is unknown, `kind` is `outcome_unknown` and `next_step` gives the safe recovery call.

### Prompt

The `mathtutor-playbook` MCP prompt returns the agent instructions for operating the MCP tools.

## Authentication and security

The server operates as a single-user, single-tenant instance backed by a single Baserunna database. Two authentication modes are supported on `/mcp`:

1. **Static bearer token:** Configured via `MT_API_KEY` and passed in `Authorization: Bearer <token>` or `?token=<token>`. The server accepts the `?token=` query parameter only for `MT_API_KEY` because proxies and servers log URLs. The key must have at least 32 characters, such as the output of `openssl rand -base64 32`.
2. **OAuth 2.1 access token:** Issued by the built-in authorization server if `MT_ADMIN_PASSWORD` is set, and passed in `Authorization: Bearer <token>`.

Either `MT_API_KEY` or `MT_ADMIN_PASSWORD` must be set at startup. Unauthenticated requests receive `401 Unauthorized` with a `WWW-Authenticate` header pointing to `/.well-known/oauth-protected-resource`. The server caches each valid access token in memory until it expires, so it reads a token from Baserunna only once. If the server cannot read an access token from Baserunna, the request receives `503 Service Unavailable` with a `Retry-After` header. A token without the format of an issued access token fails without a Baserunna request.

### Built-in OAuth 2.1 authorization server

| Path                                      | Method | Purpose                                   |
| ----------------------------------------- | ------ | ----------------------------------------- |
| `/.well-known/oauth-protected-resource`   | GET    | RFC 9728 protected resource metadata      |
| `/.well-known/oauth-authorization-server` | GET    | RFC 8414 authorization server metadata    |
| `/oauth/register`                         | POST   | RFC 7591 dynamic client registration      |
| `/oauth/authorize`                        | GET    | Renders the login form with a CSRF token  |
| `/oauth/authorize`                        | POST   | Checks `MT_ADMIN_PASSWORD`, issues a code |
| `/oauth/token`                            | POST   | Exchanges an S256 PKCE code or refresh    |

Only the metadata routes, `/oauth/register`, and `/oauth/token` send CORS headers so that browser-based MCP clients on other origins can call them. `/oauth/authorize` omits CORS headers so that external websites cannot read the sign-in form.

### Token parameters and storage

- **Entropy:** 256-bit random tokens encoded with base64url.
- **Expirations:** Single-use authorization codes expire in 60 seconds. Access tokens expire in 1 hour. Refresh tokens expire in 30 days and rotate on each refresh.
- **PKCE:** `S256` code challenge is mandatory.
- **Scope:** The server grants only the `mcp` scope. An authorization request without a scope receives `mcp`. An authorization or refresh request for another scope fails with `invalid_scope`.
- **Client registrations:** Registration is open so that MCP connectors can register themselves. An unused client registration expires after 1 day. The first token grant removes the expiry so that the connector stays registered. A token request from an unknown client fails with `401 invalid_client` so that the connector registers again.
- **Tables:** The server stores OAuth state in `oauth_clients`, `oauth_codes`, `oauth_access_tokens`, `oauth_refresh_tokens`, `oauth_addr_counts`, and `oauth_keys`. Code, token, and count rows expire through a native row TTL. Rows in `oauth_keys` and clients with a token grant do not expire. Code and token rows are keyed by the SHA-256 digest of the secret so that the database does not store raw bearer secrets.
- **Sign-in form:** The form displays the registered client name and redirect URI host for the owner to verify before signing in. Each form includes a 15-minute CSRF token signed with a key in `oauth_keys`, so the form remains valid across restarts, key rotations, and machines. If an authorization table upgrade replaces the signing key, the server reloads the key before rejecting a form. Expired forms render a fresh form and prompt for the password again. Every `/oauth/authorize` response includes `Content-Security-Policy: frame-ancestors 'none'` and `X-Frame-Options: DENY` to prevent framing.

### Limits per client address

- **Incorrect passwords:** After 5 incorrect passwords from an address, subsequent sign-in attempts must wait. The delay starts at 1 second, doubles on each failure up to 15 minutes, and resets after a correct password or 1 hour without failures. Delayed requests receive `429 Too Many Requests` with a `Retry-After` header without checking the password.
- **Rejected bearer tokens:** After 20 rejected bearer tokens on `/mcp` from an address within 1 minute, subsequent bearer tokens from that address receive `429 Too Many Requests` with a `Retry-After` header until the minute ends. This in-memory counter is separate from the password limit so that failed tokens from a shared egress address cannot delay sign-in. `MT_API_KEY` and cached access tokens bypass this limit.
- **Registration:** An address can register up to 10 clients per hour. Further registrations receive `429 Too Many Requests` with a `Retry-After` header.
- **Sign-in and token requests:** An address can send up to 60 requests per minute across `/oauth/authorize` and `/oauth/token`. Further requests receive `429 Too Many Requests` with a `Retry-After` header from the in-memory counter before reading the database.
- **Client address:** The server uses the TCP peer address by default, or the header configured via `MT_CLIENT_IP_HEADER` or `--client-ip-header` behind a proxy that overwrites that header. `fly.toml` sets `Fly-Client-IP`. All IPv6 addresses in a `/64` network share a bucket.
- **Storage:** Password and registration counts persist in `oauth_addr_counts` across restarts and machines, while an in-memory cache of up to 10,000 addresses absorbs bursts before querying the database.

## Deployment

`Dockerfile` and `fly.toml` deploy `mt mcp` to Fly.io via `.github/workflows/deploy-mcp.yml`.

Set `MT_PUBLIC_URL` or `--public-url` to the public base URL of the server, such as `https://mathtutor.fly.dev`. OAuth metadata uses this URL as the issuer and endpoint base. `fly.toml` sets it in `[env]`, and the connector URL is `<public URL>/mcp`.

### Health checks

- **`/health`:** Returns `200 ok` without querying Baserunna. The Fly service check uses `/health` so that a Baserunna outage does not remove the server from routing or trigger restarts.
- **`/ready`:** Verifies that the database is usable and returns `200 ready` without modifying it. An existing database without learner tables is ready because the first tool call creates them. Unusable databases return `503` with the error kind, such as `{"ready": false, "kind": "config"}`, and checks that exceed 5 seconds return `503` with `kind: "database"`. The server caches each result for 10 seconds and runs a single check at a time. Both `fly.toml` and the deploy workflow check `/ready` so that broken deployments fail.

Neither route requires authentication.

Required secrets:

```sh
flyctl secrets set MT_ADMIN_PASSWORD='...'
flyctl secrets set MT_API_KEY='...'
flyctl secrets set BASERUNNA_TOKEN='...'
```

`MT_API_KEY` must have at least 32 characters, such as the output of `openssl rand -base64 32`. Set a new value with `flyctl secrets set` to rotate the key and restart the machines, and then update each client.

`fly.toml` sets `BASERUNNA_URL` in `[env]`, and `BASERUNNA_DB` defaults to `mathtutor`. Set `BASERUNNA_TOKEN` to a database-limited key so that a leaked server secret cannot delete the database.

Give the server key its own rate limit below the Baserunna user limit of 500 requests per second per node. A lower limit prevents a flood of server requests from exhausting the user budget and blocking the CLI on other devices. Create the rate-limited server key through the Baserunna API with a session token:

```sh
curl -X POST "$BASERUNNA_URL/v1/auth/login" -H 'Content-Type: application/json' \
  -d '{"email": "...", "password": "..."}'   # returns a session token
curl -X POST "$BASERUNNA_URL/v1/api-keys" -H "Authorization: Bearer <session token>" \
  -H 'Content-Type: application/json' \
  -d '{"name": "mt mcp", "db": "mathtutor", "rate_limit": 50}'
```

`rate_limit` sets the maximum requests per second on each Baserunna node. The server does not create the database; if the database is missing, `/ready` fails with `kind: "config"` and tool calls report `BASERUNNA_DB`. See "Baserunna keys" in `docs/design.md`.
