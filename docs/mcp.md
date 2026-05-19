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
    target_atoms TEXT NOT NULL -- JSON array of strings
);

CREATE TABLE events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts DATETIME NOT NULL,
    kind TEXT NOT NULL,
    path_id TEXT NOT NULL REFERENCES paths(id),
    atom_id TEXT,
    quiz_id TEXT,
    payload TEXT -- JSON object
);

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

Expose the MCP server as a remote API:

- **Authentication:** Use a simple API Key (passed in an `Authorization` header or as a query parameter) to restrict access to the owner
- **HTTPS:** Serve over HTTPS (handled by hosting provider)
- **Hosting:** Deploy as a containerised app on a platform like **Fly.io** or **Railway**.

## Implementation Plan

Each item is a PR-sized task.

1.  **PR 1: Database Foundation.**
    - Update `Cargo.toml` dependencies.
    - Create `src/db.rs`: setup `libsql` connection pooling and initial schema migrations.
2.  **PR 2: Event Log SQL Migration.**
    - Refactor `src/event_log.rs` to use SQL for appending and loading events.
    - Update `src/scheduler.rs` and `src/cards.rs` if they need direct DB access.
3.  **PR 3: Overlay & Store SQL Migration.**
    - Refactor `src/overlay.rs` and `src/store.rs` to use SQL.
    - Update `Graph::load_for_path` to fetch the overlay from the database.
4.  **PR 4: AYML to SQLite Migration Tool.**
    - Implement a CLI command/utility to import existing `~/.mathtutor/` data into the new SQLite database.
5.  **PR 5: MCP Server (SSE) + Authentication.**
    - Implement `src/mcp.rs` using `rmcp`.
    - Setup `axum` server with SSE routes and API Key authentication.
6.  **PR 6: Deployment & Infrastructure.**
    - Create `Dockerfile` and Fly.io configuration.
    - Setup CI/CD for automated deployments.
