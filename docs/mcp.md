# MCP Server Design

`mt mcp` exposes the Math Tutor tools and agent playbook over the Model Context Protocol (MCP) using HTTP with Server-Sent Events (SSE).

## Architecture

- **Feature flag:** The `mcp` feature in `Cargo.toml` enables the HTTP server and `mt mcp` subcommand.
- **Transport:** Streamable HTTP / SSE via `axum` and `rmcp`.

## Interface

### Tool conventions

- **`path_id`**: Required for all path-scoped tools (`AnswerQuiz`, `UpsertLesson`, `CreateQuiz`, `UpdateQuiz`, `DeleteQuiz`, `GetNext`, `GetState`, `GetTree`, `GetSyllabus`, `SetStrategy`, `SetSubpath`, `ClearSubpath`). Optional for curriculum inspection tools (`GetItem`, `GetChildren`); when omitted, those tools return static curriculum data without path completion state.
- **Return format**: All tools return JSON. Business logic errors return a `CallToolResult` with `isError: true`.

### Prompt

The `mathtutor-playbook` MCP prompt returns the agent instructions for operating the MCP tools.

## Authentication and security

The server operates as a single-user, single-tenant instance backed by a single Baserunna database. Two authentication modes are supported on `/mcp`:

1. **Static bearer token:** Configured via `MT_API_KEY` and passed in `Authorization: Bearer <token>` or `?token=<token>`.
2. **OAuth 2.1 access token:** Issued by the built-in authorization server when `MT_ADMIN_PASSWORD` is set, and passed in `Authorization: Bearer <token>`.

At least a single authentication variable (`MT_API_KEY` or `MT_ADMIN_PASSWORD`) must be set at startup. Unauthenticated requests receive `401 Unauthorized` with a `WWW-Authenticate` header pointing to `/.well-known/oauth-protected-resource`.

### Built-in OAuth 2.1 authorization server

| Path                                      | Method | Purpose                                                                 |
| ----------------------------------------- | ------ | ----------------------------------------------------------------------- |
| `/.well-known/oauth-protected-resource`   | GET    | RFC 9728 protected resource metadata                                    |
| `/.well-known/oauth-authorization-server` | GET    | RFC 8414 authorization server metadata                                  |
| `/oauth/register`                         | POST   | RFC 7591 dynamic client registration                                    |
| `/oauth/authorize`                        | GET    | Renders the password login form with an HMAC-signed CSRF token          |
| `/oauth/authorize`                        | POST   | Verifies `MT_ADMIN_PASSWORD` in constant time and issues an auth code   |
| `/oauth/token`                            | POST   | Exchanges an authorization code (with S256 PKCE) or refresh token       |

### Token parameters and storage

- **Entropy:** 256-bit random tokens encoded with base64url.
- **Expirations:** Authorization codes expire in 60 seconds (single-use); access tokens expire in 1 hour; refresh tokens expire in 30 days (rotated on each refresh).
- **PKCE:** `S256` code challenge is mandatory.
- **Tables:** `oauth_clients`, `oauth_codes`, `oauth_access_tokens`, and `oauth_refresh_tokens` are stored in the Baserunna database using native row TTLs for expiration.

## Deployment

`Dockerfile` and `fly.toml` deploy `mt mcp` to Fly.io via `.github/workflows/deploy-mcp.yml`.

Required secrets:

```sh
flyctl secrets set MT_ADMIN_PASSWORD='...'
flyctl secrets set MT_API_KEY='...'
flyctl secrets set BASERUNNA_TOKEN='...'
```

`BASERUNNA_URL` defaults to `https://baserunna.fly.dev` and `BASERUNNA_DB` defaults to `mathtutor`.
