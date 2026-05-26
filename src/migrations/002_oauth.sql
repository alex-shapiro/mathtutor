-- OAuth authorization-server state for `mt mcp`. Every token issued here
-- has the same authority as a static `MT_API_KEY` request: this is a
-- single-user server, the AS exists only to satisfy Claude clients that
-- require OAuth on remote connectors. Rows can be freely deleted any
-- time the operator wants to invalidate all sessions (or via expiry
-- sweeps, when one's implemented).

CREATE TABLE oauth_clients (
    client_id     TEXT PRIMARY KEY,
    client_name   TEXT,
    redirect_uris TEXT NOT NULL,   -- JSON array of strings
    created_at    DATETIME NOT NULL
);

-- Short-lived (≤60s) one-shot codes minted by `/oauth/authorize` after
-- the operator passes the password challenge. `used_at` enforces
-- single-use; `code_challenge` carries the PKCE S256 hash the matching
-- `/oauth/token` call must verify against.
CREATE TABLE oauth_authorization_codes (
    code                  TEXT PRIMARY KEY,
    client_id             TEXT NOT NULL REFERENCES oauth_clients(client_id),
    redirect_uri          TEXT NOT NULL,
    scope                 TEXT,
    code_challenge        TEXT NOT NULL,
    code_challenge_method TEXT NOT NULL,   -- always 'S256' (OAuth 2.1 mandate)
    resource              TEXT,            -- RFC 8707 resource indicator, if any
    expires_at            DATETIME NOT NULL,
    used_at               DATETIME
);

CREATE TABLE oauth_access_tokens (
    token      TEXT PRIMARY KEY,
    client_id  TEXT NOT NULL REFERENCES oauth_clients(client_id),
    scope      TEXT,
    expires_at DATETIME NOT NULL
);
CREATE INDEX idx_oauth_access_tokens_exp ON oauth_access_tokens(expires_at);

CREATE TABLE oauth_refresh_tokens (
    token      TEXT PRIMARY KEY,
    client_id  TEXT NOT NULL REFERENCES oauth_clients(client_id),
    scope      TEXT,
    expires_at DATETIME NOT NULL
);
CREATE INDEX idx_oauth_refresh_tokens_exp ON oauth_refresh_tokens(expires_at);
