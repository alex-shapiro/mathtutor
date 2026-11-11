# MCP Server Design

`mt mcp` exposes the Math Tutor tools and agent playbook over the Model Context Protocol using HTTP with Server-Sent Events.

## Architecture

- **Feature flag:** The `mcp` feature in `Cargo.toml` enables the HTTP server and `mt mcp` subcommand.
- **Transport:** Streamable HTTP and SSE via `axum` and `rmcp`.

## Interface

### Tool conventions

- **`path_id`**:
  - Required for all path-scoped tools.
  - Optional for the curriculum inspection tools `get_item` and `get_children`. If omitted, those tools return static curriculum data without path completion state.
- **Return format**: All tools return JSON. A read tool returns the same record as the matching CLI command: `get_paths` as `mt path list`, `get_state` as `mt path state`, `get_tree` as `mt path tree`, `get_syllabus` as `mt path syllabus`, `get_item` as `mt graph show`, and `get_children` as `mt graph list`. `get_next` returns the `mt path next` record, and its `next_step` is an MCP tool call. Business logic errors return a `CallToolResult` with `isError: true` and a JSON object with `error` and `kind`. If the commit status is unknown, `kind` is `outcome_unknown`, and `next_step` gives the safe next call.

### Prompt

The `mathtutor-playbook` MCP prompt returns the agent instructions for operating the MCP tools.

## Authentication and security

The server operates as a single-user, single-tenant instance backed by a single Baserunna database. Two authentication modes are supported on `/mcp`:

1. **Static bearer token:** Configured via `MT_API_KEY` and passed in `Authorization: Bearer <token>` or `?token=<token>`.
2. **OAuth 2.1 access token:** Issued by the built-in authorization server if `MT_ADMIN_PASSWORD` is set, and passed in `Authorization: Bearer <token>`.

Either `MT_API_KEY` or `MT_ADMIN_PASSWORD` must be set at startup. Unauthenticated requests receive `401 Unauthorized` with a `WWW-Authenticate` header pointing to `/.well-known/oauth-protected-resource`. If the server cannot read an access token from Baserunna, the request receives `503 Service Unavailable` with a `Retry-After` header.

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
- **Scope:** A refresh request can keep or narrow the scope of its grant. A broader scope fails with `invalid_scope`.
- **Tables:** `oauth_clients`, `oauth_codes`, `oauth_access_tokens`, and `oauth_refresh_tokens` are stored in the Baserunna database using native row TTLs for expiration. Code and token rows are keyed by the SHA-256 digest of the secret, so the database does not hold usable bearer secrets.

## Deployment

`Dockerfile` and `fly.toml` deploy `mt mcp` to Fly.io via `.github/workflows/deploy-mcp.yml`.

Required secrets:

```sh
flyctl secrets set MT_ADMIN_PASSWORD='...'
flyctl secrets set MT_API_KEY='...'
flyctl secrets set BASERUNNA_TOKEN='...'
```

`BASERUNNA_URL` defaults to `https://baserunna.fly.dev` and `BASERUNNA_DB` defaults to `mathtutor`. Set `BASERUNNA_TOKEN` to a key that is limited to the database, so that a leak of the server secret cannot delete the database. The database must exist before the server starts with such a key, because a limited key cannot create a database. See "Baserunna keys" in `docs/design.md`.
