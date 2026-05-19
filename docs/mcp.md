# MCP Server Design: mathtutor

This document outlines the transition of `mathtutor` from a standalone CLI tool to a Model Context Protocol (MCP) server.

## Overview

The current `mathtutor` is a CLI tool (`mt`) that manages a math curriculum DAG and tracks user progress via local AYML files. To make it more accessible to LLM agents (like Claude Desktop), we will wrap the core logic in an MCP server.

## Architecture: Unified Binary (`mt mcp`)

The MCP server is integrated into the existing `mt` tool.

- **Feature Flag:** `mcp` ff in `Cargo.toml` gates `axum`, `rmcp`, `tokio` and enables the `mt mcp` command
- **Transport:** SSE over HTTP for remote access
- **Protocol:** MCP JSON-RPC 2.0 via `rmcp`

## Tool Mapping

All CLI tools are ported to MCP tools; no changes needeed.

## Data Persistence & Backup: SQLite + Turso

The server will be hosted remotely and accessed from multiple devices and so we must migrate the storage layer from local AYML files to SQLite. The server will use Turso for sync/backup.

- **Storage:** Migrate from AYML files to a local SQLite database that syncs with a remote Turso instance
- **Sync:** Turso's `libsql` driver handles background synchronization automatically
- **Implementation:**
  - Rewrite `src/store.rs`, `src/event_log.rs`, and `src/overlay.rs` to use SQL queries.
  - The server runs a local SQLite file for low-latency reads and the driver pushes updates to Turso

### SQL Schema

```sql
CREATE TABLE paths (
    id TEXT PRIMARY KEY,
    goal TEXT NOT NULL,
    created_at DATETIME NOT NULL,
    target_atoms TEXT NOT NULL CHECK (json_valid(target_atoms))
);

CREATE TABLE events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts DATETIME NOT NULL,
    kind TEXT NOT NULL,
    path_id TEXT NOT NULL REFERENCES paths(id),
    atom_id TEXT,
    quiz_id TEXT,
    payload TEXT CHECK (payload IS NULL OR json_valid(payload))
);

-- Indexes for performance
CREATE INDEX idx_events_path ON events(path_id);
CREATE INDEX idx_events_atom ON events(atom_id);

CREATE TABLE overlay_lessons (
    path_id TEXT NOT NULL REFERENCES paths(id),
    atom_id TEXT NOT NULL,
    body TEXT NOT NULL,
    PRIMARY KEY (path_id, atom_id)
);

CREATE TABLE overlay_quizzes (
    path_id TEXT NOT NULL REFERENCES paths(id),
    atom_id TEXT NOT NULL,
    quiz_id TEXT NOT NULL,
    difficulty TEXT NOT NULL,
    kind TEXT, -- NULL for free_text
    question TEXT NOT NULL,
    answer TEXT NOT NULL,
    rubric TEXT,
    PRIMARY KEY (path_id, quiz_id)
);

CREATE TABLE overlay_removed_quizzes (
    path_id TEXT NOT NULL REFERENCES paths(id),
    quiz_id TEXT NOT NULL,
    PRIMARY KEY (path_id, quiz_id)
);
```

## Remote Access & Security

### Authentication Fallback Logic

To support both modern MCP clients and legacy `EventSource` (SSE) browsers, the server implements a sequential authentication check:

1.  **Authorization Header:** First, try to extract a `Bearer` token from the `Authorization` header.
2.  **Query Parameter Fallback:** If no header is present, look for a `token` query parameter
3.  **Precedence:** If both are provided, the `Authorization` header takes precedence
4.  **Error Handling:** If no valid token is found, the server returns a `401 Unauthorized`.

_Security Note:_ While query parameters are less secure due to potential logging, this fallback is required for standard SSE client compatibility. Users are encouraged to use HTTPS and rotate keys regularly.

## Data Persistence: SQLite + Turso

### libSQL Sync Strategy

- **Mode:** `libsql::Builder::new_synced_database` (Embedded Replica with Offline Writes).
- **Initialization:**
  - If `TURSO_URL` and `TURSO_AUTH_TOKEN` are set: Enable remote sync.
  - Otherwise: Fallback to a standard local SQLite database.
- **Sync Trigger:**
  - **Server Mode:** A background tokio task will call `db.sync()` every 5 minutes (or on startup/shutdown).
  - **CLI Mode:** `db.sync()` is called at the end of every command that modifies state (store, answer, etc.) to ensure the remote is updated immediately.

## Implementation Plan (PR-Sized Tasks)

1.  **PR 1: Database Foundation.** 
    - Update `Cargo.toml` dependencies (`libsql`, `tokio`, `serde_json`).
    - Create `src/db.rs`: setup `libsql` connection pooling, `json_valid` checks, and initial schema migrations.
    - Implement the "Local vs. Synced" initialization logic.
2.  **PR 2: Event Log SQL Migration.**
    - Refactor `src/event_log.rs` to use SQL for appending and loading events.
    - Update `src/scheduler.rs` and `src/cards.rs` if they need direct DB access.
3.  **PR 3: Overlay & Store SQL Migration.**
    - Refactor `src/overlay.rs` and `src/store.rs` to use SQL.
    - Update `Graph::load_for_path` to fetch the overlay from the database.
4.  **PR 4: AYML to SQLite Migration Tool.**
    - Implement `mt migrate-from-ayml` CLI command.
    - **Idempotency:** Use `INSERT OR IGNORE` and unique constraints to allow repeated runs.
5.  **PR 5: MCP Server (SSE) + Authentication.**
    - Implement `src/mcp.rs` using `rmcp`.
    - Setup `axum` server with SSE routes and API Key authentication (Header + Query Fallback).
6.  **PR 6: Deployment & Infrastructure.**
    - Create `Dockerfile` and Fly.io/Railway configuration.
    - Setup CI/CD for automated deployments.
